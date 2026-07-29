# Phase 0 spike: `pdx-ls` distribution

Status: concluded as a release contract; actual GitHub Release upload is a release-phase smoke
test.

> Historical note: RFC 0014 superseded the bundled-rules and `--rules` portions of this spike.
> Official binaries now embed the first-party rules. The artifact matrix and server resolution
> order remain current.

## Decision

The Zed client resolves the server in this order:

1. explicit development executable path;
2. `pdx-ls` on `PATH`;
3. a cached platform artifact downloaded from the project release.

The server is never built by the extension. Under the current RFC 0014 design, the rules database
is embedded in the official binary and the extension passes no external rules path.

## Artifact matrix

| Target | Archive | Binary |
| --- | --- | --- |
| Linux x86_64 | `pdx-ls-v<version>-x86_64-unknown-linux-gnu.tar.gz` | `pdx-ls` |
| Linux aarch64 | `pdx-ls-v<version>-aarch64-unknown-linux-gnu.tar.gz` | `pdx-ls` |
| macOS x86_64 | `pdx-ls-v<version>-x86_64-apple-darwin.tar.gz` | `pdx-ls` |
| macOS arm64 | `pdx-ls-v<version>-aarch64-apple-darwin.tar.gz` | `pdx-ls` |
| Windows x86_64 | `pdx-ls-v<version>-x86_64-pc-windows-msvc.zip` | `pdx-ls.exe` |

Each release publishes a SHA-256 checksum file. The client validates the exact target name,
version, archive checksum, and extracted executable before caching it in the extension work
directory. A failed download or incompatible artifact is an actionable error, not a fallback to
network-loaded rules.

## What Phase 0 verified

- The root Cargo workspace has a `pdx-ls` binary target that can be built independently of the
  editor package.
- `pdx-ls --version` runs without a workspace-specific path; Phase 2 replaces the lifecycle-only
  placeholder with the stdio JSON-RPC runtime described in RFC 0009.
- The artifact names map directly to Rust target triples and have a deterministic lookup key.

The following remain release/Phase 1 work: cross-compiling all targets, signing/uploading release
assets, implementing the Zed API download adapter, and running an install/launch smoke test on
each supported platform.
