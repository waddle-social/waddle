#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"

export GITHUB_TOKEN="${CI_GITHUB_TOKEN:?missing GITHUB_TOKEN}"
export GITHUB_ACTOR="${CI_GITHUB_ACTOR:?missing GITHUB_ACTOR}"
export GITHUB_REF_TYPE="${CI_GITHUB_REF_TYPE:?missing GITHUB_REF_TYPE}"
export GITHUB_REF_NAME="${CI_GITHUB_REF_NAME:?missing GITHUB_REF_NAME}"
: "${CUENV_ARCH:?missing CUENV_ARCH}"

case "${CUENV_ARCH}" in
  amd64) ;;
  *) echo "unsupported CUENV_ARCH=${CUENV_ARCH}" >&2; exit 1 ;;
esac

export FULL_SHA="$(git -C "${repo_root}" rev-parse HEAD)"
if [[ ! "${FULL_SHA}" =~ ^[a-f0-9]{40}$ ]]; then
  echo "refusing to publish from non-canonical git commit ${FULL_SHA}" >&2
  exit 1
fi
worktree_status="$(git -C "${repo_root}" status --porcelain --untracked-files=normal)"
if [[ -n "${worktree_status}" ]]; then
  echo "refusing to publish dirty working-tree bytes as commit ${FULL_SHA}" >&2
  printf '%s\n' "${worktree_status}" >&2
  exit 1
fi

export RELEASE_DIGEST_DIR="${repo_root}/target/digests"
rm -rf -- "${RELEASE_DIGEST_DIR}"
mkdir -p -- "${RELEASE_DIGEST_DIR}"

printf '%s\n' "${GITHUB_TOKEN}" | docker login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
printf '%s\n' "${GITHUB_TOKEN}" | helm registry login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
printf '%s\n' "${GITHUB_TOKEN}" | oras login ghcr.io -u "${GITHUB_ACTOR}" --password-stdin

"${script_dir}/publish-container-image.sh"
"${script_dir}/publish-helm-chart.sh"
"${script_dir}/publish-extension-modules.sh"
"${script_dir}/publish-gitops-release.sh"

# Mutable convenience tags are deliberately last. No failed digest-addressed
# image, chart, extension, or GitOps verification may move them.
"${script_dir}/publish-container-tags.sh"
