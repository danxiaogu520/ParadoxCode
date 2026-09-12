# Releasing ParadoxCode

ParadoxCode publishes the server and editor extension as one immutable version. The release
workflow does not make a GitHub Release public until the five native server archives, their five
checksum sidecars, the VSIX, and the release sweep summary have all been built and verified.
Visual Studio Marketplace publication remains a separate manual step.

## One-time publisher and repository setup

1. Create or verify the `paradoxcode` publisher in Visual Studio Marketplace. The stable extension
   identity is `paradoxcode.paradoxcode-vscode`.
2. Enable immutable releases, secret scanning, push protection, Dependabot alerts, and Dependabot
   security updates in the GitHub repository settings.
3. Keep the `main-protection` and `version-tags` repository rulesets active. `main` requires the
   `Conclusion` check; `v*` tags cannot be updated or deleted after creation.
4. Keep the dedicated `paradoxcode-sweep` runner and repository Actions variables healthy. See
   [`docs/runner-recovery.md`](docs/runner-recovery.md).

## Before tagging

1. Merge the release-preparation pull request and wait for its `Conclusion` check to succeed on
   `main`.
2. Confirm `Cargo.toml`, `editors/vscode/package.json`, and `editors/vscode/package-lock.json`
   carry the intended version and that `CHANGELOG.md` has a dated entry for it.
3. Run `cargo tools gates`. Do not tag if any local group fails.
4. Package the VSIX with `npm --prefix editors/vscode run package`, install it into a clean VS Code
   profile, trust an EU4 Mod workspace, and open an EU4 file. Verify that the ParadoxCode status
   item shows its check mark without configuring the server, completion and diagnostics work, and
   the output reports a checksum-verified automatic installation.
5. Review the generated VSIX contents and confirm no Vanilla files, caches, credentials, or
   development artifacts are present.

## Publish

Create and push an annotated version tag from the reviewed commit on `main`:

```bash
git tag -a v0.4.0 -m "ParadoxCode 0.4.0"
git push origin v0.4.0
```

The tag workflow performs these gates in order:

1. Verify stable SemVer, annotated-tag form, ancestry from `main`, a successful `Conclusion` check,
   and the absence of an existing GitHub Release for the tag.
2. Build and checksum all five native server archives, and build and audit the VSIX.
3. Pass the packaged Windows archive to the self-hosted release sweep, compare its diagnostic
   fingerprint with the previous release, and save `sweep-summary.json`.
4. Reassemble and verify the twelve-file release payload.
5. Create a draft Release, compare every uploaded asset name with the verified payload, and only
   then publish it. Repository release immutability locks the published assets and tag.

The workflow intentionally has no overwrite path. A run may be manually dispatched for an
unpublished annotated tag, but it cannot replace or modify an existing Release.

## Recover an interrupted draft

If the final publication job is interrupted after it creates a draft, inspect that draft and its
workflow logs. Published releases must never be edited or deleted. For a draft only:

1. Confirm the Release is still marked Draft and record the failed workflow URL.
2. Delete the incomplete draft in the GitHub UI without deleting or moving its tag.
3. Re-run the Release workflow for the same unpublished tag.

If a Release has already become public, fix any defect in a new patch version instead of modifying
the published version.

## Verify the public release

1. Confirm the Release is immutable and carries exactly five server archives, five `.sha256`
   sidecars, `paradoxcode-vscode-<version>.vsix`, and `sweep-summary.json`.
2. Confirm the Release workflow's provenance, build, extension, sweep, and publish jobs all passed.
3. Upload the VSIX manually to Visual Studio Marketplace. Subscribe from a clean VS Code profile
   and repeat the installation smoke test without relying on a populated global server cache.
4. Record public links and known limitations in the release notes and milestone.
