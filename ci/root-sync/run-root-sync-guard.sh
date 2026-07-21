#!/usr/bin/env bash
set -euo pipefail

: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE must be set}"
repo_root=${GITHUB_WORKSPACE}
marker=${repo_root}/ci/root-sync/.root-sync-guard-passed
rm -f -- "${marker}"

cuenv_bin=$(command -v cuenv || true)
if [[ ! ${cuenv_bin} = /* || ! -x ${cuenv_bin} ]]; then
	echo "root sync guard requires an absolute executable cuenv" >&2
	exit 1
fi

"${cuenv_bin}" exec -p "${repo_root}/ci/root-sync" env ROOT_SYNC_CUENV="${cuenv_bin}" \
	bash -ceu 'cd "$1"; bun test scripts/check-root-sync-drift.test.ts; bun scripts/check-root-sync-drift.mjs' \
	bash "${repo_root}/server"
touch "${marker}"
