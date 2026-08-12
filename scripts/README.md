# Development scripts

## Whole-Current-Mod diagnostics

`diagnose-current-mod.mjs` drives the real `pdx-ls` stdio JSON-RPC server and checks every EU4
source file below a Current Mod. Rules remain the embedded first-party `rules/eu4` source; the
Vanilla input is an existing local `.pdxindex` cache, so the report exercises the same parser, HIR,
resolution, and diagnostics code used by Zed. Indexed Current Mod files are diagnosed in bounded
batches through `pdx/workspaceDiagnostics`, avoiding an unnecessary overlay reparse. With
`--vanilla-source`, selected source text is sent through the bounded `pdx/textDiagnostics` request
while definitions and references continue to resolve against the matching Vanilla cache. The script
prints progress and checkpoints partial JSON and Markdown reports every 128 files.

```bash
bash scripts/diagnose-current-mod.sh \
  --mod /path/to/mod \
  --vanilla-cache /path/to/vanilla.pdxindex
```

To validate Vanilla itself, or split a large reproducible run across independent LSP processes:

```bash
node scripts/diagnose-current-mod.mjs \
  --vanilla-source "/path/to/Europa Universalis IV" \
  --vanilla-cache /path/to/vanilla.pdxindex \
  --shard-count 4 --shard-index 0
```

Run shard indices `0` through `3` with separate output directories. Sharding is stable round-robin
over the sorted selected paths; `--path-prefix` can narrow a diagnostic run to one logical subtree.

The command writes an ignored `diagnostic-reports/current-mod-<timestamp>.json` machine report and
matching Markdown report. It exits with status `1` when an error is found (use `--fail-on warning`
to fail on warnings too, or `--fail-on none` for collection-only runs). Run
`node scripts/diagnose-current-mod.mjs --help` for all options and environment variables.

## Quoted Script inventory

`audit-quoted-scripts.mjs` scans a local EU4 tree for multiline quoted property values without
printing or storing their payload text. It reports aggregate key counts, workspace scripted-macro
candidates from the scanned definitions, and, when given the first-party semantic source,
candidate `quoted_script` matches by key and structural parent path:

```bash
node scripts/audit-quoted-scripts.mjs \
  --source "/path/to/Europa Universalis IV" \
  --rules rules/eu4/semantic-rules.json
```

Use `--json` for the payload-free per-location inventory. Candidate counts are review aids rather
than proof of semantic context. The exact-parent-path count is a conservative lower bound that
excludes rules containing dynamic `<...>` path segments, because a lexical suffix cannot establish
their semantic container. Scripted effect/trigger carriers are intentionally not first-party rule
matches: the separate workspace-macro candidate count is derived from top-level definitions and
their `$PARAM$` tokens in the scanned source tree. Runtime templates infer the actual standalone
quoted parameters. An engine helper absent from both fixed rules and workspace semantics remains
opaque. Dynamic members still come from the workspace/Vanilla index, and the audit never modifies
rule source.
