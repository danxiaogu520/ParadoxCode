# Security Policy

ParadoxCode takes security seriously. The language server processes untrusted content: EU4 mod
files you open in an editor, downloaded dependency indexes, and cached Vanilla scans. Please report
security issues privately so they can be fixed before they are disclosed.

## Reporting a vulnerability

**Do not open a public issue** for security problems. Report them privately through one of these
channels:

1. **Preferred:** open a [private security advisory](https://github.com/danxiaogu520/ParadoxCode/security/advisories/new)
   on this repository. Only repository maintainers can see it.
2. **Alternative:** send a direct message to maintainer `@danxiaogu520` on GitHub if you cannot
   use the advisory workflow.

Please include in your report:

- The affected version(s) and platform(s).
- A minimal, reproducible description (file contents, commands, or steps).
- The impact as you understand it (crash, resource exhaustion, path escape, code execution, ...).
- Whether the vulnerability is public already.

You will receive an acknowledgement within **7 days**. We will keep you informed as the issue is
triaged and fixed, and we ask that you give us **reasonable time to fix and release** before any
public disclosure. See [GitHub's coordinated disclosure guidance](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
for what that typically looks like.

## Scope

The following areas are in scope:

- `pdx-ls` and the `pdx` CLI: parsing and indexing of untrusted files, cache handling, archive
  extraction, path handling, and resource limits.
- Server bootstrap and checksum verification in the VS Code and Zed extensions.
- Rule/packaging artifacts: integrity metadata, manifest checks, and redistribution boundaries.
- Dependencies with known, exploitable CVEs affecting the shipped binary or extensions.

The following are **out of scope** and should be filed as regular issues instead:

- Incorrect diagnostics, completions, or rule data (functional correctness, not security).
- Unsupported game files that are intentionally ignored.
- Issues that require already-compromised credentials or physical machine access.

## How vulnerabilities are handled

1. **Triage** — severity, affected versions, and initial fix plan.
2. **Fix** — a patch is prepared with tests; it ships through the normal quality gates and release
   workflow.
3. **Disclosure** — after the fix is released, an advisory is published and credited to the
   reporter unless they prefer to stay anonymous.

## Security design notes

The repository is built on the assumption that the server runs on the user's machine with the
user's privileges. Defense-in-depth measures already in place include:

- The workspace forbids `unsafe` code entirely.
- User-supplied paths and contents are validated; the formatter refuses to rewrite unsafe CSTs.
- Scanning is bounded by file size, nesting depth, path escaping, and resource consumption limits.
- Server downloads are checksum-verified (SHA-256) with restricted extraction and bounded streaming.
- The runtime never imports external rule sources; first-party rules are validated before use.
- CI runs advisory scans (`cargo-deny`) and the release workflow uses least-privilege OIDC tokens.