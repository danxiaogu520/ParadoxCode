# RFC 0014: First-party rule source and compiler

- Status: Accepted
- Date: 2026-07-22
- MVP: EU4 v0.1
- Supersedes: CWT importer (removed) and the authority/maintenance decisions in RFC 0004 and RFC 0013

> 2026-08-06 amendment: `rules/eu4.pdxrules` is no longer a repository or embedded-binary
> input. The official server embeds the validated first-party JSON source bundle and materializes
> the SQLite runtime artifact in a user-local cache. The JSON source remains the sole authority.

## Decision

ParadoxCode owns and maintains one strict, versioned EU4 rule source under `rules/eu4/`. This
source tree is the sole authority for static file classification, semantic matching, cardinality,
symbol/reference metadata, documentation, and resolution policies. The selected game profile
remains the authority for algorithmic interpretation of those rows and runtime-derived facts such
as workspace symbols, scope-link evaluation, control-flow/iterator interpretation, and parser token
extraction. A profile must not duplicate a static rule row when the artifact provides it.

`.cwt` files are prohibited as rule inputs. The repository does not provide a CWT parser,
importer, fallback, synchronization command, or runtime compatibility mode. CWTools material may
be studied as historical research, but it is neither an oracle nor a build, test, release, or
maintenance dependency.

## Pipeline

```text
rules/eu4/*.json (first-party authority)
              |
              +--------------------------+
              |                          |
              v                          v
       pdx-bake CLI                embedded source bundle
   developer/release checks                 |
              |                            v
              |                 pdx-ls first-party provider
              |                            |
              +-------------> user-local SQLite cache
                                           |
                                           v
                              read-only runtime RuleSet
```

The shared compiler accepts only the fixed first-party JSON layout. Unknown fields, missing files,
duplicate stable identities, invalid cardinality, invalid severity, mismatched type identities,
and generated artifact round-trip differences fail compilation. `pdx-bake` writes artifacts to a
caller-selected developer/release path; `pdx-ls` writes only a validated artifact to its user-local
cache and never accepts that cache as an authority.

## Source layout

- `manifest.json`: `source_format_version`, `game_id`, `target_game_version`;
- `catalog.json`: file categories, symbol descriptors, normalized records;
- `semantic-rules.json`: ordered semantic rule alternatives;
- `enum-values.json`: static enum members;
- `type-root-keys.json`: type root selectors;
- `type-root-scopes.json`: initial scopes by type and root;
- `type-descriptors.json`: path, wrapper, name, and type-selection metadata;
- `localisation-bindings.json`: type-instance localisation key templates and explicit-field
  mappings, including required/optional, subtype, and data-driven subtype-condition metadata.

Source format changes require a version increment and an explicit migration. Stable identities
must not be regenerated merely because files are reordered. Generated artifacts are never edited
by hand.

Source format 2 renamed the generic key/value parser identity from `pdx-script` to `script`.
Source format 3 added the first-party type-instance localisation binding source. Source format 4
adds data-driven subtype conditions for those bindings. Source format 5 adds the optional
`semantic-rules.deprecated` field. These migrations do not change existing EU4 file category
identities, matchers, resolution policies, or semantic rule identities.

## Runtime authority

Official binaries embed the first-party JSON source bundle and construct an immutable `RuleSet` from
a user-local SQLite artifact. On startup the server validates the source, computes its canonical
`rule_hash`, and accepts a cache only when schema, `game_id`, and logical contents match. A missing,
malformed, or stale cache is regenerated in a temporary file and installed only after round-trip
validation. Compilation failure is a server/source distribution defect; the runtime does not fall
back to an old hash or search for another rule source.

The server accepts no rule arguments, environment variable, initialization option, project setting,
search path, download, or user override. The Zed extension carries no semantic rules. Server and
rule source versions move together.

## Version maintenance

The source manifest records the supported EU4 version. A game update is handled by a reviewed
source change, original regression fixtures, artifact regeneration, and a new server release.
There is no automatic extraction from game files and no historical-version selector in v0.1.

## Verification

Every rule change must pass:

1. strict source decoding and source invariants;
2. deterministic canonical `rule_hash` computation;
3. generated SQLite round-trip and foreign-key validation;
4. manifest schema/hash/checksum verification in a temporary artifact directory;
5. embedded source provider identity/hash and user-cache rebuild tests;
6. affected analysis and real JSON-RPC regression tests;
7. a clean server launch without any external rule file or committed SQLite artifact.

## Rejected alternatives

- CWT as a flexible or community-editable source: retains an unwanted compatibility language.
- One-time or recurring CWT import: keeps a second authority and makes regeneration depend on an
  external model.
- Direct manual SQLite editing: is difficult to review and cannot provide a safe source schema.
- User-supplied rule databases or JSON source paths: creates unsupported semantic and security states.
- Embedding a generated SQLite file: adds a large generated repository/release input without making JSON
  less authoritative.
- Hand-maintained Rust tables: couples game data to engine implementation and produces poor diffs.
