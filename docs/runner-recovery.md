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
Git, GitHub CLI, Node.js 24, rustup, and the stable Rust toolchain on `PATH`. The workflow uses the
Windows PowerShell 5.1 shell included with supported Windows versions; PowerShell 7 is not required.
The trusted workflow uses a process-scoped execution-policy bypass for GitHub's generated step
scripts, so the machine-wide execution policy does not need to be relaxed.

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

## Host-enforced job guard

The workflow-level caller/ref condition is defense in depth, not the machine's authorization
boundary: pull requests can change workflow files. Before bringing the runner online, copy
`.github/runner-hooks/paradoxcode-sweep-guard.sh` from reviewed `main` to
`C:/actions-runner-hooks/paradoxcode-sweep-guard.sh`, outside the runner application and work
directories. Configure this line in `C:/actions-runner/.env`:

```text
ACTIONS_RUNNER_HOOK_JOB_STARTED=C:/actions-runner-hooks/paradoxcode-sweep-guard.sh
```

Restrict both locations so only administrators and SYSTEM can modify them; grant the runner
service account read/execute access to the hook and read access to `.env`. Restart the runner
service after any `.env` change. GitHub runs this pre-job hook before repository steps, and a
nonzero exit rejects the job. The hook permits only:

- `sweep.yml` or `release.yml` manually dispatched from `refs/heads/main`; and
- `release.yml` triggered by a stable protected `vMAJOR.MINOR.PATCH` tag.

Everything else, including pull-request refs and alternate workflow paths, is denied by the host
before checkout or project code can run. Validate both an allowed health check and a deliberately
denied non-main dispatch whenever the guard changes.

## Health check

Before a release, open the Release sweep workflow, select `main`, and run it manually with an empty
artifact name. The privileged job rejects every other ref and every caller except the release
workflow. The preflight verifies tools, absolute path variables, EU4 directory markers,
permissions, and at least 10 GiB of free cache-drive space. A successful direct run builds the
current `main` revision and uploads diagnostics as workflow artifacts; it does not modify a GitHub
Release.

## Rebuild procedure

1. Remove the old runner registration in GitHub if the machine is permanently unavailable.
2. Provision a trusted Windows x64 host and install the required tools and licensed EU4 data.
3. Register a repository runner using a fresh, short-lived registration token, install it as a
   service, and add the `paradoxcode-sweep` label.
4. Install and permission the host-enforced job guard, configure `.env`, and restart the service.
5. Create a dedicated empty sweep-cache directory outside the repository checkout.
6. Update the four repository Actions variables for the new service-account paths.
7. Run the manual health check and inspect `release-sweep-diagnostics` before tagging a release.

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
