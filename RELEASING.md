# Releasing ParadoxCode

ParadoxCode releases the server and editor extension as one immutable version. The
[distribution manifest](editors/vscode/server-distribution.json) owns the five native targets,
archive layouts and size limits. GitHub publication promotes an already verified candidate;
it does not rebuild the server or VSIX. Licensed game files and local Vanilla output never
enter Actions or public releases. Marketplace publication remains a separate manual step.

## Publisher and repository setup

- Keep the stable extension identity `paradoxcode.paradoxcode-vscode` and publisher `paradoxcode`.
- Keep immutable GitHub releases, secret scanning, push protection and the `main-protection`
  and `version-tags` rulesets enabled. `Conclusion` remains the required merge check;
  formal `v*` tags cannot be moved or deleted.
- Workflow changes require appropriate `workflow` permission. Check that capability before
  preparing process changes; normal publishing uses short-lived `GITHUB_TOKEN` permissions.
- Keep Marketplace credentials outside the repository and GitHub build jobs.

## Prepare the reviewed source

1. Create a release-preparation PR. Update the workspace and extension version, the npm lockfile
   and both Rust lockfiles. External package versions are not rewritten as workspace versions.
2. Move the intended changes into a dated `CHANGELOG.md` section. That section becomes the
   release notes; the comparison baseline is the last publicly published stable Release,
   excluding unpublished tags and drafts.
3. Run the affected local groups from [CONTRIBUTING.md](CONTRIBUTING.md#validation). Run
   `cargo run --locked -p tools --no-default-features -- ci production-audit` before reserving
   a formal version. High-severity advisories fail; an unavailable audit endpoint is also a
   failure after bounded retries.
4. Run local Vanilla acceptance when semantics change. Keep game-derived output local and
   reduce defects to repository-owned fixtures.
5. Merge only after the reduced core `Conclusion` succeeds. Before preparing a release, dispatch
   Full CI on main and wait for **Full conclusion** to succeed:

   ```sh
   gh workflow run ci.yml --ref main
   ```

Full CI must test the exact main commit selected for the candidate. If main changes, validate the
newer commit or select the already validated exact SHA, subject to the control-definition match.
Full CI retains all quality jobs, an attempt-specific receipt and validated VSIX for 90 days.
Receipts bind source/tree, workflow/run, coverage and digest. Quick CI and validation-only branch
pushes cannot provide release authorization. Failed, cancelled, incomplete, forked, expired,
non-main or mismatched evidence is rejected. Skipped jobs are never sufficient. Rerun complete
manual Full CI if required artifacts are missing; receipts cannot borrow another attempt.

## Build and seal a candidate

From the reviewed main workflow, dispatch:

```sh
gh workflow run release-candidate.yml --ref main -f source=main
```

`source` may also be an exact reviewed commit SHA reachable from main. The dispatch fixes the
source identity; a later main update cannot silently change the candidate. Candidate control
files must match the dispatched workflow.

The read-only candidate workflow:

1. Verifies main CI evidence, all workspace/extension/lockfile versions and version availability.
2. Performs a fresh production dependency audit before expensive platform builds.
3. Builds all five native release targets once, checks each binary version, LSP startup, embedded
   rules and diagnostics on owned input, then packages archives with checksums.
   Lightweight `tools --no-default-features` packaging does not compile HIR/IDE
   or embed game rules a second time.
4. Downloads the original CI VSIX, verifies its producer and digest, and reuses those exact bytes.
5. Verifies the complete inventory, archive contracts, VSIX identity and release notes. Seals
   `candidate.json` with source/tree/policy, CI origin, producer run/attempt, sizes and SHA-256.

The sealed `release-candidate-ATTEMPT` Actions artifact retains the complete public payload
and notes for 90 days. No formal tag or GitHub Release is created during candidate preparation.
Failed candidates can be corrected without consuming a formal version number.

The bounded startup check uses `ci candidate-smoke --binary PATH` without linking analyzer
libraries. Public download and Marketplace installation are checked after those entry points exist.

## Promote the verified payload

After the entire candidate workflow succeeds, use its run ID:

```sh
gh workflow run release.yml --ref main -f candidate_run=RUN_ID
```

Promotion executes trusted main control code. It accepts only a successful current attempt
from the expected candidate workflow in this repository. Downloaded data is bounded, hashed,
flat and non-executable; it cannot change the control program.

Before any irreversible action, promotion rechecks candidate provenance, main ancestry, CI
origin, versions, control definitions, all file digests/layouts and a fresh dependency audit.
The audit uses the lockfile without installing packages or running lifecycle scripts, and
receives no publication credential.

It creates or resumes a draft under `candidate-RUN-ATTEMPT`, outside the formal `v*` namespace.
It uploads the unchanged verified files and compares uploaded names, sizes, GitHub digests,
source identity and notes. This staging draft is never publicly published. Promotion then refreshes
the current successful Full CI and candidate attempts for the exact source, and verifies the VSIX
bytes against its CI receipt, before reserving a formal tag. Invalidated evidence stops publication.

Do not rerun Full CI or candidate preparation during promotion. GitHub has no atomic check-and-tag
operation; this final refresh narrows that race but cannot eliminate it.

Only after every upload and verification succeeds does it create/push the annotated formal tag
for the exact candidate commit, then reassign the prepared draft to that tag and publish it as
immutable and latest. These are the final mutations. Tag pushes do not start builds or another
CI suite. Published releases, assets and formal tags are never replaced.

## Recover interruptions

- For a failure before tag creation, correct the candidate source or retry transient infrastructure.
- For an interrupted staging upload, redispatch promotion with the same candidate run. Matching
  draft assets are preserved and only missing assets are uploaded. No formal tag has been reserved.
- For an interruption between formal tag creation and public publication, redispatch the same
  candidate. The tag must still point to its source; the complete draft is published without reupload.
- Mismatched draft source, notes or assets fail without replacement. A published staging draft is
  also refused; only a formal version may become public.
- A retry after successful publication verifies the identical immutable payload and returns success.
- For a corrupted draft, verify it is still unpublished, record the failed run and delete only that
  draft through normal maintainer recovery before retrying. Never delete or move a formal tag.
  Unused `candidate-RUN-ATTEMPT` references, if present, are temporary and may be cleaned up only
  after confirming they are unrelated to a public Release.
- Source or release metadata changes after a formal tag is reserved require a new patch version.
- Missing, expired or rerun-attempt-mismatched candidate evidence requires a new successful candidate;
  an existing formal tag can only be reused for its original exact source commit.

## Verify public delivery

1. Confirm the Release workflow succeeded, the public Release is immutable and all eleven assets
   match the sealed inventory: five server archives, five checksum files and the VSIX.
2. Download and verify the published files. From a separate VS Code profile with an empty server
   cache, install the VSIX and check automatic download, checksum-backed startup, completion and
   actual editor diagnostics.
3. For Marketplace delivery, sign in to the existing `paradoxcode` publisher, upload the released
   VSIX, wait for validation and verify the public version. Install that Marketplace version in
   another clean profile and repeat the delivery checks.
4. Record public links and verification outcomes in the release issue or local acceptance record.
   Keep licensed content, raw game reports and credentials out of public records.
