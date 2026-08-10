# Development scripts

## Whole-Current-Mod diagnostics

`diagnose-current-mod.mjs` drives the real `pdx-ls` stdio JSON-RPC server and checks every EU4
source file below a Current Mod. Rules remain the embedded first-party `rules/eu4` source; the
Vanilla input is an existing local `.pdxindex` cache. Each file is opened through the normal LSP
path, so the report exercises the same parser, HIR, resolution, and diagnostics code used by Zed.

```bash
bash scripts/diagnose-current-mod.sh \
  --mod /path/to/mod \
  --vanilla-cache /path/to/vanilla.pdxindex
```

The command writes an ignored `diagnostic-reports/current-mod-<timestamp>.json` machine report and
matching Markdown report. It exits with status `1` when an error is found (use `--fail-on warning`
to fail on warnings too, or `--fail-on none` for collection-only runs). Run
`node scripts/diagnose-current-mod.mjs --help` for all options and environment variables.
