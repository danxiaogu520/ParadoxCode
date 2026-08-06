# RFC 0013: Embedded first-party source and release ownership

- Status: Accepted
- Date: 2026-07-21
- MVP: EU4 v0.1
- Supersedes: the runtime/distribution decisions in RFC 0001, 0004, 0009, and 0010

> 2026-08-06 amendment: the generated SQLite artifact is no longer embedded or committed. The
> official server embeds the first-party JSON source bundle and materializes a validated SQLite
> artifact in the user cache on first use or source-hash mismatch. This amendment preserves the
> no-user-override boundary while removing generated SQL from the repository.

## Context

The original design made the Zed extension own `eu4.pdxrules` and launch the server with
`pdx-ls --rules <path>`. That separation allowed rules to change independently from the server, but
it also created a runtime input that the product does not intend to support. ParadoxCode has one
first-party EU4 rules authority, no user rule override, no historical rule selector, and no
third-party profile contract for v0.1.

An external path therefore adds failure modes without providing a supported capability: a missing,
damaged, mismatched, or user-replaced database can change analysis behavior; extension and server
versions can drift; and Windows/WSL testing must translate an extra path across environments.

## Decision

`rules/eu4/` is the repository's developer-maintained authority. `pdx-bake` and the runtime source
provider use the same strict compiler core to validate it. The official `pdx`/`pdx-ls` binaries embed
the first-party JSON source bundle, not a generated SQLite file.

Official runtime entry points:

- load the embedded first-party JSON source bundle and compute its canonical logical `rule_hash`;
- look up a user-local SQLite artifact cache and use it only when schema, game identity, and hash
  match;
- compile the embedded source into a temporary SQLite artifact when the cache is missing, stale,
  corrupt, or mismatched, validate the complete round-trip, and atomically install it;
- do not accept `--rules`, initialization options, environment variables, project configuration, or
  external source paths that replace rules;
- do not search for, download, or update a rules database independently;
- keep the resulting `RuleSet` immutable and shared;
- treat failure to compile the embedded first-party source as a server distribution/build defect,
  not as permission to use an old or user-supplied rule set.

The separation between data and engine remains intact. `pdx-rules` owns the game-neutral source
bundle compiler, SQLite schema, loader, canonical hash, and runtime API. `pdx-game::eu4` owns the
embedded source bundle, user-cache provider, and EU4 profile. CLI and LSP binaries compose those
components; editor extensions do not interpret or carry semantic rules.

The exact source-bundle byte-loading mechanism is an implementation detail. A later compressed
bundle or compact runtime representation is allowed only if it preserves the canonical logical
model, hash, provenance, and validation behavior.

## Authority and integrity

Embedding is not a substitute for release authenticity: anyone can fork and rebuild the project.
Official authority is established by protected repository/release permissions, immutable versioned
artifacts, checksums, and eventual code or manifest signing.

The following remain mandatory:

- schema version and `game_id` validation;
- canonical logical `rule_hash` computed from the embedded source;
- first-party source version, target game version, and redistribution review;
- deterministic regeneration and invariant tests;
- the rule hash recorded in Vanilla cache metadata;
- identical embedded source logical content across platform builds of one release;
- atomic user-cache publication with no stale-rule fallback after a source mismatch.

The original CWT corpus remains a local, one-time maintenance input and is never embedded or
distributed. EU4 Vanilla files and user caches remain excluded from source and releases.

This maintenance-input exception was removed by RFC 0014 on 2026-07-22. The embedded first-party
source bundle and every generated runtime artifact are compiled exclusively from `rules/eu4/`; no
CWT input is permitted.

## Distribution

The Zed extension is compiled and distributed by the Zed extension registry. It resolves the target
platform, downloads the matching versioned `pdx-ls` artifact from the official project release,
verifies its SHA-256 checksum, caches it in the extension work directory, and launches it without a
rules argument. The server carries the first-party JSON source bundle and creates its own user-local
SQLite artifact cache.

Server version and rules source version still move together. Updating first-party rules requires a
new server release; changing only grammar/query assets may require only an extension update.

## Tradeoffs

The generated SQLite artifact is approximately 22 MiB, but it is no longer committed or embedded in
every native binary. The first server use pays the JSON validation and SQLite materialization cost;
subsequent starts validate the embedded source, compute its canonical hash, and load the user-local
artifact when schema, game identity, and logical contents match. The cache is versioned by schema,
game identity, and canonical hash and can be rebuilt safely.

The source bundle remains embedded so this does not create a user rule override or a network rule
update surface. Users still cannot experiment with alternative rules through official runtime entry
points; v0.1 does not expose a third-party rule or game-profile compatibility contract.

## Migration

1. Expose a tested embedded EU4 JSON source bundle and shared source compiler.
2. Add a user-local first-party SQLite artifact cache keyed and validated by canonical hash.
3. Make `pdx-ls` compile on cache miss/mismatch and load only the validated SQLite artifact for
   runtime queries.
4. Remove the committed `rules/eu4.pdxrules` artifact and make release checks regenerate it in a
   temporary directory.
5. Keep `--rules` and all external rule/source override paths absent from user-facing entry points.
6. Update configuration, help text, smoke tests, and release documentation.
7. Run a clean Windows installation test without Rust, SQLite, or a separate rules file.

Each migration step must keep the workspace buildable and preserve existing semantic behavior.

## Rejected alternatives

- Keep an unsigned external rules path: preserves an unsupported injection surface and version drift.
- Require a separately downloaded JSON or signed rules file: retains unnecessary installation and
  compatibility states when the source can be embedded in the server.
- Embed the generated SQLite artifact in every binary: increases release size and requires a tracked
  generated file without improving the runtime model.
- Convert the database into hand-maintained Rust tables: destroys the auditable data pipeline and
  makes rule review substantially harder.
- Design a general plugin ABI now: no second supported game/profile has demonstrated the required
  behavior contract.
