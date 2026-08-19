# Releasing ParadoxCode

This checklist keeps the `0.1.x` server and editor extensions versioned and published as one
release. Creating a tag is an external release operation: review the exact commit and complete the
account prerequisites before pushing it.

## One-time publisher setup

1. Create or verify the `paradoxcode` publisher in Visual Studio Marketplace. The stable extension
   identity is `paradoxcode.paradoxcode-vscode`.
2. Configure Marketplace trusted publishing for GitHub repository `danxiaogu520/ParadoxCode` and
   workflow `release.yml`. The release job uses `vsce publish --oidc`, so no Azure DevOps
   organization or long-lived `VSCE_PAT` secret is required.
3. Fork `zed-industries/extensions` in preparation for the Zed registry PR. Zed publication cannot
   happen solely from this repository: the registry requires a reviewed submodule entry.

## Before tagging

1. Confirm `Cargo.toml`, `editors/vscode/package.json`, `editors/vscode/package-lock.json`,
   `editors/zed/Cargo.toml`, and `editors/zed/extension.toml` all carry the intended version.
2. Run `bash scripts/check-quality-gates.sh`. Do not tag if any group fails.
3. Package the VSIX with `npm --prefix editors/vscode run package`, install it into a clean VS Code
   profile, trust an EU4 Mod workspace, and open an EU4 file. Verify that `PDX ●` appears without
   configuring `pdx-ls`, completion/diagnostics work, and the output reports a checksum-verified
   automatic installation.
4. Install `editors/zed` as a Zed dev extension in a clean profile and verify that it downloads the
   same release version and starts the server without editor settings.
5. Review the generated VSIX contents and confirm no Vanilla files, local caches, credentials, or
   development artifacts are present.

## Publish

Create and push an annotated version tag only after the commit on `main` has passed CI:

```bash
git tag -a v0.1.0 -m "ParadoxCode 0.1.0"
git push origin v0.1.0
```

The tag workflow builds and verifies all five native `pdx-ls` archives, creates the immutable
GitHub Release, packages the VSIX, attaches it to that release, and publishes the same VSIX to the
Visual Studio Marketplace through OIDC trusted publishing. The VS Code job deliberately waits for
the server release because a fresh extension install immediately downloads that matching asset.

For an already-pushed tag, use the workflow's manual dispatch input with the exact tag (for example
`v0.1.0`). This runs the current release workflow while checking out the immutable tagged source.

For Zed, open the required PR against `zed-industries/extensions` after the tagged commit is public.
Use this repository as the HTTPS submodule, set `path = "editors/zed"`, and set the registry version
to the exact `extension.toml` version. Zed publishes the extension after that PR is reviewed and
merged.

## Verify the public release

1. Confirm all five server archives and checksum sidecars plus the VSIX are attached to the GitHub
   Release.
2. Subscribe to the public Marketplace extension from a clean VS Code profile and repeat the
   installation smoke test. Do not rely on a previously populated global server cache.
3. After the Zed registry PR merges, install ParadoxCode from Zed's Extension Gallery and repeat the
   clean-profile server-start smoke test.
4. Only then update the README's pre-release status and record the public links and any known
   limitations in the release notes.
