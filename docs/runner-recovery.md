# Release sweep runner recovery

The release sweep runs only on a trusted Windows x64 self-hosted runner because it needs a licensed
local Europa Universalis IV installation and stable hardware for release-to-release comparisons.
Pull-request code never runs on this runner.

## Required runner configuration

Register the runner at repository scope as a Windows service and apply these labels:

- `self-hosted`
- `Windows`
- `X64`
- `paradoxcode-sweep`

The service account needs read and execute access to the EU4 installation, and read/write access to
its Cargo, rustup, Actions work, temporary, and dedicated sweep-cache directories. Install current
Git, GitHub CLI, Node.js 24, rustup, and the stable Rust toolchain on `PATH`.

Configure these repository Actions variables; paths must be absolute:

| Variable | Purpose |
| --- | --- |
| `PDC_SWEEP_CARGO_HOME` | Cargo home used by the runner service account |
| `PDC_SWEEP_RUSTUP_HOME` | rustup home used by the runner service account |
| `PDC_SWEEP_CACHE_DIR` | Dedicated disposable sweep cache directory |
| `PDC_SWEEP_VANILLA_SOURCE` | EU4 installation containing `eu4.exe` and the game data directories |

None of these values is a credential. Do not place GitHub, Steam, or Marketplace credentials in
repository variables or the checked-out workspace. The workflow receives a short-lived GitHub
token automatically.

## Health check

Before a release, open the Release sweep workflow and run it manually with an empty artifact name.
The preflight verifies tools, variables, EU4 directory markers, permissions, and at least 10 GiB of
free cache-drive space. A successful direct run builds the current revision and uploads diagnostics
as workflow artifacts; it does not modify a GitHub Release.

## Rebuild procedure

1. Remove the old runner registration in GitHub if the machine is permanently unavailable.
2. Provision a trusted Windows x64 host and install the required tools and licensed EU4 data.
3. Register a repository runner using a fresh, short-lived registration token, install it as a
   service, and add the `paradoxcode-sweep` label.
4. Create a dedicated empty sweep-cache directory outside the repository checkout.
5. Update the four repository Actions variables for the new service-account paths.
6. Run the manual health check and inspect `release-sweep-diagnostics` before tagging a release.

Do not copy Cargo targets, Actions work directories, GitHub tokens, or user editor caches from the
old machine. The persistent performance baseline is the previous immutable Release's
`sweep-summary.json`, which the workflow downloads automatically.

## Failure handling

- If the runner is offline, the release remains queued and no public Release is created.
- If environment validation fails, repair the runner or variables and re-run the workflow.
- If the diagnostics fingerprint changes, inspect the uploaded report, make the intentional code
  or baseline change through a pull request, and create a new tag only after review.
- Never bypass the sweep for an ordinary release. Security emergency handling must be documented
  according to `GOVERNANCE.md` and should use a new patch version.
