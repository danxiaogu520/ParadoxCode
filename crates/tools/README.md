# Repository tools

`tools` owns developer commands, local performance measurements, semantic audits, real-process
LSP/MCP contracts, extension validation, documentation and release/CI orchestration. Run it
from the repository root:

```sh
cargo run --locked -p tools -- --help
cargo run --locked -p tools -- gates core-fast
cargo run --locked -p tools -- gates vscode
cargo run --locked -p tools -- lsp test
```

Use a command's `--help` for inputs. `--root` selects a repository; defaults are derived from
the compiled workspace. An explicit `--server` pins the tested binary and fails if it is missing.
Without it, real-server tests build the current debug binary before running.

## Commands and ownership

| Command | Implementation | Purpose |
| --- | --- | --- |
| `rules check / fmt / schema / bake` | [editor.rs](src/editor.rs), rule compiler | Rule authoring through the compiler's existing Rust CLI |
| `audit diagnose / sweep / baseline` | [audit/client.rs](src/audit/client.rs) | Real LSP diagnostics, phase/resource measurements and frozen query probes |
| `audit errors / diff` | [audit/resources.rs](src/audit/resources.rs), [audit/compare.rs](src/audit/compare.rs) | Per-error resource evidence and complete diagnostic/SQLite comparisons |
| `audit completions` | [audit/completions.rs](src/audit/completions.rs) | Complete IDE candidates before LSP truncation, with a bounded-LSP equality check |
| `audit templates` | [audit/templates](src/audit/templates/mod.rs) | Owned text/call/finite-domain references and frozen real-LSP editing observations |
| `perf bench / baseline / ab / profile` | [perf/workflow.rs](src/perf/workflow.rs) | Warmup, repetitions, frozen binaries, comparisons and external profilers |
| `perf probe / compare` | [perf/client.rs](src/perf/client.rs) | Actual LSP initialization, readiness, open/edit publication and interactive queries |
| `perf memory` | [perf/memory.rs](src/perf/memory.rs) | Sequential measured-child peak RSS, phase RSS and diagnostic signatures |
| `perf init / status / import-corpus / control` | [perf/workflow.rs](src/perf/workflow.rs) | Environment checks, owned local inputs and native comparison workflows |
| `editor check / compile / test / package` | [editor.rs](src/editor.rs) | Static contracts, translations, TypeScript compilation, behavior tests and VSIX packaging |
| `lsp test` | [e2e.rs](src/e2e.rs) | Real-binary startup, semantic tokens, mission payloads, document changes, queries and MCP |
| `fuzz smoke` | [ci.rs](src/ci.rs) | Bounded nightly runs over every registered target |
| `gates / check / documentation / release / ci` | Adjacent Rust modules | Local test groups, policy, generated views and CI/release workflows |

The LSP transport client lives in [pdc](../pdc/src/client.rs) so the shipped MCP adapter can share
it without depending on developer tools. The MCP product entry is `paradoxcode mcp`; see
[its guide](../pdc/README.md#mcp-server). Tools invoke installed `node`, TypeScript, VSCE, VS Code, Cargo,
profilers and GitHub CLI with argument vectors. These remain external runtimes/toolchains.

The extension's actual JavaScript behavior tests remain under
[editors/vscode/test](../../editors/vscode/test). `tools editor test unit` runs those tests and
the LSP/MCP contracts; `contract` adds package inventory checks; `ci` also produces a VSIX.
`host` explicitly launches the VS Code/Electron suite. Normal CI groups do not download Electron.

## Diagnostics and semantic evidence

```sh
cargo run --locked -p tools -- audit diagnose \
  --server target/debug/paradoxcode --mod /path/to/mod \
  --vanilla-cache /path/to/vanilla.pdcindex --output target/diagnostic-reports/current
cargo run --locked -p tools -- audit baseline \
  --server target/debug/paradoxcode --vanilla-source /path/to/corpus \
  --vanilla-cache /path/to/vanilla.pdcindex --output target/performance-results/current
cargo run --locked -p tools -- audit errors \
  --report target/performance-results/current/project-report.json \
  --installation /path/to/game --output target/performance-results/error-review
cargo run --locked -p tools -- audit diff \
  --before target/performance-results/before --before-cache /path/to/before.pdcindex \
  --after target/performance-results/after --after-cache /path/to/after.pdcindex \
  --output target/performance-results/comparison.json
cargo run --locked -p tools -- audit completions \
  --baseline target/performance-results/current/baseline-latest.json \
  --cache /path/to/vanilla.pdcindex --output target/performance-results/completions
```

The server supplies its active rule hash, version and arena counts through `pdc/analyzerInfo`.
Reports freeze that identity with the binary digest. Rule baking and audit commands use no
sidecar manifest. Readers still accept historical report field names and validate their identities
against the selected server, cache and report.

Persistent caches use the workspace LSP release version. Updating the version rebuilds semantic
indexes and misses older parse entries. Same-version development rebuilds require explicit index
regeneration and clearing the parse cache before behavioral validation.

Error IDs preserve the previous Python serializer, including occurrence numbers for identical
diagnostics, so saved decisions stay valid. Decisions require an explanation and evidence;
unmatched decisions, incomplete reports and inconsistent totals fail. Frozen resource reuse
requires `--prior-review`, `--prior-cache` and `--cache`, identical source roots/fingerprints and
an untampered prior report. It inherits evidence only for exact diagnostic identities.

Comparisons retain duplicate counts, exact messages, moved-position guards, truncation and
unpaired prefix samples. Equal counts do not approve semantics. Full completion exports verify
the golden input digest and equality of the first 512 IDE candidates with the recorded LSP set.
Current exports call IDE directly. An explicitly selected historical `--repo`/`--mode legacy`
uses an isolated Rust compatibility harness without modifying that checkout's source.

## Template references and editing baselines

```sh
cargo run --locked -p tools -- audit templates --check-only \
  --output target/performance-results/template-reference
cargo run --locked --release -p tools -- audit templates \
  --server /path/to/frozen/paradoxcode --samples 20 \
  --output target/performance-results/template-owned
cargo run --locked --release -p tools -- audit templates \
  --server /path/to/frozen/paradoxcode --samples 20 \
  --vanilla-cache /path/to/frozen/vanilla.pdcindex \
  --output target/performance-results/template-with-vanilla
```

The owned [text matrix](src/audit/templates/cases.json) records expected text, Missing/Empty/Hole,
guards, source ranges, resource limits and evidence status. The reference substitutes lossless text
independently of HIR replay. Its one-pass project profile does not rescan inserted markers, and
all unobserved EU4 engine behavior stays explicitly unverified. Concrete named-call cases use an
explicit stack and the ordinary parser after substitution; they retain frame/caller provenance,
test finite same-name calls and limits, and check the expanded invalid terminal against ordinary
first-party diagnostics. Scalar argument blocks are supported; general block-argument and engine
termination semantics remain pending. The [finite relation matrix](src/audit/templates/finite.json)
exhaustively joins assignments before projecting a parameter, including incompatible witnesses.

These references define the declared project support profile. A runnable EU4 installation is not
required for this engineering workflow. Engine evidence status is separate from analysis validity:
unverified engine behavior does not invalidate ordinary queries under adopted project conventions.
Interpretations outside that support profile retain local conditions or unknown results rather
than silently choosing the reference's provisional behavior as an engine guarantee.

The [editing trace](src/audit/templates/queries.json) freezes source and UTF-16 cursor anchors.
Each state queries completion, hover, definition, references, rename, code actions, symbols,
semantic tokens, inlay hints, text diagnostics and completion resolve when a candidate exists.
Rename rejection is retained as an observed RPC result. Every other request failure fails the run;
missing/deduplicated diagnostic publication is recorded without inventing a zero latency.
The first request is separated from warmed samples. Repeated-response stability, fixture and
binary hashes, rule/version identity, optional cache identity and sampled process RSS are recorded.
RSS sampling may require local process-statistics permission; missing samples stay explicit.

`reference.json`, `call-reference.json` and `finite-reference.json` verify owned expectations.
`baseline.json` records current LSP behavior, including known omissions and deliberate future
behavior changes; it is not a correctness oracle or a claim that the redesign is complete.
Re-run with identical frozen binaries, cache and fixtures. Preserve the previous output directory
when comparing revisions. Failed runs cannot leave the prior baseline marked successful.

## Performance measurements

```sh
cargo run --locked -p tools -- perf bench --repeat 3 -- -p engine --bench index_cache --locked
cargo run --locked -p tools -- perf probe --no-cache
cargo run --locked -p tools -- perf compare --workspace /path/to/mod --cache /path/to/cache.pdcindex
cargo run --locked -p tools -- perf baseline before --skip-sweep --repeat 3
cargo run --locked -p tools -- perf ab before --bench-only --repeat 3 --fail-over 10
cargo run --locked -p tools -- perf profile bench:index_cache --flavor samply
cargo run --locked -p tools -- perf memory \
  --before /path/to/before/mem_probe --after /path/to/after/mem_probe \
  --source /path/to/corpus --output target/performance-results/memory \
  --require-identical-diagnostics
```

Benchmark logs, individual runs and medians are retained. A/B rejects mismatched benchmark
inventories and records diagnostic work differences separately. Timings still need repeated
runs and semantic review. Failed or missing samples remain explicit; unobserved/deduplicated
publications never become zero-latency measurements.

`perf sweep --cold` uses a new dedicated cache path. Native cache construction is recorded as
`cache_build_ms`; the report explicitly identifies fresh construction followed by server loading.
Existing user caches are retained. Compare runs with the same cache protocol, inputs, binary
build mode and filesystem state.

Memory pairs use the Unix platform `time` utility's measured-child `wait4` peak and `ps` for
that child's phase RSS. Sampler processes are excluded from that peak. Periodic LSP RSS in
probe/compare/sweep is a separate sampled measurement. Windows supports the periodic process
sampler; exact memory pairs require Unix. Profiling requires `samply` or Linux `perf`.

Machine settings belong in ignored `target/perf/config.local.toml`, for example:

```toml
source = "/path/to/local/corpus"
cache = "/path/to/local/cache.pdcindex"
vanilla_origin = "/path/to/installed/game"
edg_origin = "/path/to/local/mod"
control_repo = "/path/to/native/control/checkout"
control_rules = "/path/to/control/rules"
control_binary = "/path/to/control/binary"
```

CLI paths take precedence. Existing `PDC_VANILLA_SOURCE`, `PDC_VANILLA_ORIGIN`,
`EDG_WORKSHOP_SOURCE`, `CWTOOLS_REPO`, `CWTOOLS_RULES`, `CWTOOLS_NATIVE_BIN`, diagnostic-client
and performance-client environment inputs are supported. Shell configuration is not executed.
`init --install` explicitly opts into the Linux apt adapter; ordinary checks do not install tools.

## Local outputs and validation

Corpus inputs remain in `data/`. Generated reports, caches, profiles, baselines and release
staging remain under ignored `target/`. Output guards reject traversal and symlink escapes.
Licensed corpus text and machine paths stay local; remote CI runs only owned fixtures and
benchmarks. Findings needing a decision remain pending until reviewed.

```sh
cargo test --locked -p tools
cargo run --locked -p tools -- gates audit
cargo run --locked -p tools -- documentation check
```

CI calls the same native operations. Workflow YAML retains triggers, toolchain setup, runner
matrices, permissions and dependencies. Release publication still verifies tag provenance,
successful CI, complete checksummed assets and the draft upload before making it public.
The PR autosync job executes code from the trusted default branch when using its write token.
