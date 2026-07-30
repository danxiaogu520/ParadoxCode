# RFC 0013: Embedded first-party rules and release ownership

- Status: Accepted
- Date: 2026-07-21
- MVP: EU4 v0.1
- Supersedes: the runtime/distribution decisions in RFC 0001, 0004, 0009, and 0010

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

`rules/eu4/` is the repository's developer-maintained authority. `pdx-bake` validates it and
generates the auditable `rules/eu4.pdxrules` artifact, which is embedded into the official `pdx`
and `pdx-ls` binaries as a build input.

Official runtime entry points:

- load only the embedded first-party EU4 rules;
- do not accept `--rules`, initialization options, environment variables, or project configuration
  that replace rules;
- do not search for, download, or update a rules database independently;
- keep the resulting `RuleSet` immutable and shared;
- treat failure to load the embedded artifact as an internal build/release defect, not a user
  configuration problem.

The separation between data and engine remains intact. `pdx-rules` owns the game-neutral schema,
loader, canonical hash, and runtime API. `pdx-game-eu4` owns the EU4 profile and the first-party
embedded artifact provider. CLI and LSP binaries compose those components; editor extensions do not
interpret or carry semantic rules.

The exact byte-loading mechanism is an implementation detail. The first implementation should favor
auditable behavior over a custom format. A later compact release representation is allowed only if
it preserves the canonical logical model, hash, provenance, and validation behavior.

## Authority and integrity

Embedding is not a substitute for release authenticity: anyone can fork and rebuild the project.
Official authority is established by protected repository/release permissions, immutable versioned
artifacts, checksums, and eventual code or manifest signing.

The following remain mandatory:

- schema version and `game_id` validation;
- canonical logical `rule_hash`;
- first-party source version, target game version, and redistribution review;
- deterministic regeneration and invariant tests;
- the rule hash recorded in Vanilla cache metadata;
- identical embedded logical content across platform builds of one release.

The original CWT corpus remains a local, one-time maintenance input and is never embedded or
distributed. EU4 Vanilla files and user caches remain excluded from source and releases.

This maintenance-input exception was removed by RFC 0014 on 2026-07-22. The embedded artifact is
now compiled exclusively from `rules/eu4/`; no CWT input is permitted.

## Distribution

The Zed extension is compiled and distributed by the Zed extension registry. It resolves the target
platform, downloads the matching versioned `pdx-ls` artifact from the official project release,
verifies its SHA-256 checksum, caches it in the extension work directory, and launches it without a
rules argument.

Server version and rules version now move together. Updating first-party rules requires a new server
release; changing only grammar/query assets may require only an extension update.

## Tradeoffs

The current SQLite artifact is approximately 24 MiB, so embedding increases every native binary and
may temporarily duplicate memory while constructing `RuleSet`. This cost is accepted for v0.1 in
exchange for a single self-contained runtime artifact and a smaller configuration/security surface.
Compression or a compact read-only representation requires measured release-size or startup-memory
evidence before adoption.

Embedding also removes user experimentation with alternative rules. That is intentional: v0.1 does
not expose a third-party rule or game-profile compatibility contract.

## Migration

1. Add a tested embedded EU4 rules provider without changing analysis APIs.
2. Make `pdx` and `pdx-ls` use that provider and verify the expected hash in release checks.
3. Remove `--rules` from user-facing CLI/LSP entry points and tests.
4. Remove the bundled rules asset and rules-path arguments from the Zed extension.
5. Implement versioned, checksummed server download and cache behavior.
6. Update configuration, help text, smoke tests, and release documentation.
7. Run a clean Windows installation test without Rust, SQLite, or a separate rules file.

Each migration step must keep the workspace buildable and preserve existing semantic behavior.

## Rejected alternatives

- Keep an unsigned external rules path: preserves an unsupported injection surface and version drift.
- Require a separately downloaded signed rules file: improves integrity but retains unnecessary
  installation and compatibility states.
- Convert the database into hand-maintained Rust tables: destroys the auditable data pipeline and
  makes rule review substantially harder.
- Design a general plugin ABI now: no second supported game/profile has demonstrated the required
  behavior contract.
