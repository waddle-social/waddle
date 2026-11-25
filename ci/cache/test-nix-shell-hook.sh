#!/usr/bin/env bash
set -euo pipefail

hook="$(cd "$(dirname "$0")" && pwd)/nix-shell-hook.sh"
runner="$(mktemp -d)"
trap 'rm -rf "$runner"' EXIT

environment_for_job() (
  local bootstrap="$1"
  mkdir -p "$bootstrap"
  printf '#!/bin/sh\nexit 0\n' > "$bootstrap/cuenv"
  chmod +x "$bootstrap/cuenv"
  export GITHUB_ACTIONS=true RUNNER_TEMP="$runner" CUENV_REMOTE_CACHE=https://cache.invalid
  export NIX_BUILD_TOP="$bootstrap/scratch" TMP="$bootstrap/scratch"
  export TMPDIR="$TMP" TEMP="$TMP" TEMPDIR="$TMP"
  nix_saved_PATH="$bootstrap:$PATH"
  export PATH="/nix/store/dev-shell/bin:$nix_saved_PATH"

  source "$hook"
  [[ "$NIX_BUILD_TOP" == "$bootstrap/scratch" && "$TMPDIR" == "$TMP" ]]
  [[ "$(type -P cuenv)" -ef "$bootstrap/cuenv" ]]
  [[ "$PATH" == /nix/store/dev-shell/bin:* && "$PATH" != *"$bootstrap"* ]]
  printf '%s\n' "$PATH"
)

first="$(environment_for_job "$runner/waddle-cuenv.first")"
second="$(environment_for_job "$runner/waddle-cuenv.second")"
[[ "$first" == "$second" ]]

# Local developer shells retain their host tools and Nix scratch directory.
unset GITHUB_ACTIONS CUENV_REMOTE_CACHE
export NIX_BUILD_TOP="$runner/local-scratch"
original_path="$PATH"
source "$hook"
[[ "$PATH" == "$original_path" && "$NIX_BUILD_TOP" == "$runner/local-scratch" ]]
