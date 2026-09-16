# Releasing ParadoxCode

ParadoxCode publishes the server and editor extension as one immutable version. The release
workflow uses only repository-owned source and fixtures: it builds five native server archives,
their five checksum sidecars, and one VSIX, verifies the eleven-file payload, then publishes it
once. Licensed game files and local Vanilla sweep output never enter GitHub Actions or Releases.

Visual Studio Marketplace publication remains a separate manual acceptance step. Validation
ownership across the complete development lifecycle is documented in
[`docs/validation.md`](docs/validation.md).

## One-time publisher and repository setup

1. Create or verify the `paradoxcode` publisher in Visual Studio Marketplace. The stable extension
   identity is `paradoxcode.paradoxcode-vscode`.
2. Enable immutable releases, secret scanning, push protection, Dependabot alerts, and Dependabot
   security updates in the GitHub repository settings.
3. Keep the `main-protection` and `version-tags` repository rulesets active. `main` requires the
   `Conclusion` check; `v*` tags cannot be updated or deleted after creation.
4. Keep Marketplace credentials outside the repository and GitHub build jobs.

## Release-preparation pull request

1. Choose the version according to the release scope. Keep fixes and maintenance separate from
   unrelated high-risk features.
2. Update `Cargo.toml`, `editors/vscode/package.json`, and
   `editors/vscode/package-lock.json`; update the root and fuzz lockfiles where required.
3. Move the relevant `CHANGELOG.md` entries from Unreleased into a dated version section. Update
   current-version user documentation without duplicating historical release prose.
4. Run the affected local groups from `docs/validation.md`. Package and install the VSIX into a
   clean VS Code profile when extension startup, installation, or distribution changed.
5. If diagnostic, rule, parser/HIR, index, or workspace-query behavior changed, run the local
   Vanilla sweep as development evidence. Keep every generated report local and reduce any defect
   found to a repository-owned regression fixture.
6. Merge only after the pull request's required `Conclusion` check succeeds. Wait for the same
   check to succeed on the resulting `main` commit.

Local checks and sweep output do not authorize a release. The tagged commit's remote `Conclusion`
is the source of truth.

## Publish

Create and push an annotated version tag from the reviewed commit on `main`:

```bash
git tag -a vX.Y.Z -m "ParadoxCode X.Y.Z"
git push origin vX.Y.Z
```

The tag workflow performs these gates in order:

1. Verify stable SemVer, annotated-tag form, ancestry from `main`, a successful `Conclusion` check,
   and the absence of an existing GitHub Release for the tag.
2. Build and checksum all five native server archives and verify that every binary reports the tag
   version.
3. Build, contract-test, audit, and package the VSIX at the same version.
4. Reassemble and verify the eleven-file release payload.
5. Create a draft Release, compare every uploaded asset name with the verified payload, and only
   then publish it. Repository release immutability locks the published assets and tag.

The workflow intentionally has no overwrite path. It can be manually rerun for an unpublished
annotated tag, but it cannot replace or modify an existing Release.

## Recover an interrupted release

First distinguish infrastructure interruption from a product defect:

- For a transient build or GitHub interruption on an unpublished tag, rerun the Release workflow.
- If the final job created an incomplete draft, confirm it is still a draft, record the failed run,
  delete only that draft without deleting or moving its tag, then rerun the workflow.
- If code or release metadata must change, repair it through a pull request and create a new patch
  tag. Protected tags are never moved or deleted.
- Published releases and their assets are never edited or replaced.

## Verify the public release

1. Confirm the Release is immutable and carries exactly five server archives, five `.sha256`
   sidecars, and `paradoxcode-vscode-<version>.vsix`.
2. Confirm the Release workflow's provenance, build, extension, payload-verification, and publish
   jobs all passed.
3. Upload the released VSIX manually to Visual Studio Marketplace. From a clean VS Code profile,
   install the Marketplace version and verify checksum-backed server installation, startup,
   completion, and diagnostics without relying on a populated global server cache.
4. Record the GitHub Release and Marketplace links, known limitations, and milestone completion in
   the release notes or release issue.
