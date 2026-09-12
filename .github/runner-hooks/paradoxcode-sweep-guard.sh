#!/usr/bin/env bash
set -euo pipefail

repository="${GITHUB_REPOSITORY:-}"
workflow_ref="${GITHUB_WORKFLOW_REF:-}"
git_ref="${GITHUB_REF:-}"
event_name="${GITHUB_EVENT_NAME:-}"

deny() {
  echo "::error title=Untrusted self-hosted runner job::$1"
  exit 1
}

if [[ "$repository" != "danxiaogu520/ParadoxCode" ]]; then
  deny "Repository is not authorized: ${repository:-<unset>}"
fi

trusted_main_ref="refs/heads/main"
trusted_sweep_workflow="danxiaogu520/ParadoxCode/.github/workflows/sweep.yml@$trusted_main_ref"
trusted_release_main="danxiaogu520/ParadoxCode/.github/workflows/release.yml@$trusted_main_ref"

if [[ "$event_name" == "workflow_dispatch" && "$git_ref" == "$trusted_main_ref" ]]; then
  if [[ "$workflow_ref" == "$trusted_sweep_workflow" || "$workflow_ref" == "$trusted_release_main" ]]; then
    echo "Runner guard accepted trusted main workflow: $workflow_ref"
    exit 0
  fi
fi

if [[ "$event_name" == "push" && "$git_ref" =~ ^refs/tags/v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  trusted_release_tag="danxiaogu520/ParadoxCode/.github/workflows/release.yml@$git_ref"
  if [[ "$workflow_ref" == "$trusted_release_tag" ]]; then
    echo "Runner guard accepted trusted release tag: $workflow_ref"
    exit 0
  fi
fi

deny "Only release.yml on a protected version tag, or release.yml/sweep.yml manually dispatched from main, may use this runner (event=${event_name:-<unset>}, ref=${git_ref:-<unset>}, workflow_ref=${workflow_ref:-<unset>})."
