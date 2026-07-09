#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server_root="$(cd "${script_dir}/.." && pwd)"
repo_root="$(cd "${server_root}/.." && pwd)"
repository="ghcr.io/waddle-social/waddle"
placeholder_digest="sha256:0000000000000000000000000000000000000000000000000000000000000000"

: "${FULL_SHA:?missing FULL_SHA}"
: "${CUENV_ARCH:?missing CUENV_ARCH}"
: "${RELEASE_DIGEST_DIR:?missing RELEASE_DIGEST_DIR}"

image_stream="$(nix build --print-out-paths "${repo_root}#waddle-server-image-stream")"
"${image_stream}" | docker image load

embedded_commit="$(docker run --rm "${repository}:nix" --build-commit)"
if [[ "${embedded_commit}" != "${FULL_SHA}" ]]; then
  echo "refusing to publish container with embedded commit ${embedded_commit}; expected ${FULL_SHA}" >&2
  exit 1
fi
local_image_id="$(docker image inspect --format '{{.Id}}' "${repository}:nix")"
local_rootfs="$(docker image inspect --format '{{json .RootFS.Layers}}' "${repository}:nix")"

commit_ref="${repository}:sha-${FULL_SHA}"
push_log="${RELEASE_DIGEST_DIR}/push-${CUENV_ARCH}.log"
resolve_log="${RELEASE_DIGEST_DIR}/resolve-${CUENV_ARCH}.log"
commit_ref_exists=false
if remote_manifest="$(docker buildx imagetools inspect "${commit_ref}" --format '{{json .Manifest}}' 2>"${resolve_log}")"; then
  commit_ref_exists=true
  existing_digest="$(printf '%s\n' "${remote_manifest}" | yq -r '.digest')"
  if [[ ! "${existing_digest}" =~ ^sha256:[a-f0-9]{64}$ ]] || [[ "${existing_digest}" == "${placeholder_digest}" ]]; then
    echo "invalid existing full-SHA image locator digest: ${existing_digest}" >&2
    exit 1
  fi
  docker pull "${repository}@${existing_digest}" >/dev/null
  existing_image_id="$(docker image inspect --format '{{.Id}}' "${repository}@${existing_digest}")"
  existing_rootfs="$(docker image inspect --format '{{json .RootFS.Layers}}' "${repository}@${existing_digest}")"
  if [[ "${existing_image_id}" != "${local_image_id}" ]] || [[ "${existing_rootfs}" != "${local_rootfs}" ]]; then
    echo "full-SHA image locator ${commit_ref} already exists with different config or rootfs bytes" >&2
    exit 1
  fi
elif ! grep -Eiq 'not found|manifest unknown|manifest_unknown|404' "${resolve_log}"; then
  cat "${resolve_log}" >&2
  echo "could not determine whether ${commit_ref} already exists" >&2
  exit 1
fi

if [[ "${commit_ref_exists}" == false ]]; then
  docker tag "${repository}:nix" "${commit_ref}"
  docker push "${commit_ref}" 2>&1 | tee "${push_log}"
  pushed_digest="$(sed -nE 's/^[[:space:]]*digest:[[:space:]]*(sha256:[a-f0-9]{64})([[:space:]].*)?$/\1/p' "${push_log}" | tail -n1)"
fi

remote_manifest="$(docker buildx imagetools inspect "${commit_ref}" --format '{{json .Manifest}}')"
digest="$(printf '%s\n' "${remote_manifest}" | yq -r '.digest')"
if [[ ! "${digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
  echo "invalid remote image digest for ${commit_ref}: ${digest:-<missing>}" >&2
  exit 1
fi
if [[ "${digest}" == "${placeholder_digest}" ]]; then
  echo "Refusing to publish GitOps with the all-zero image digest placeholder" >&2
  exit 1
fi
if [[ "${commit_ref_exists}" == true ]] && [[ "${existing_digest}" != "${digest}" ]]; then
  echo "full-SHA image locator ${commit_ref} moved from ${existing_digest} to ${digest} during verification" >&2
  exit 1
fi
if [[ "${commit_ref_exists}" == false ]] && [[ "${pushed_digest}" != "${digest}" ]]; then
  echo "pushed image digest ${pushed_digest:-<missing>} does not match remote ${commit_ref} digest ${digest}" >&2
  exit 1
fi

docker pull "${repository}@${digest}" >/dev/null
remote_image_id="$(docker image inspect --format '{{.Id}}' "${repository}@${digest}")"
remote_rootfs="$(docker image inspect --format '{{json .RootFS.Layers}}' "${repository}@${digest}")"
if [[ "${remote_image_id}" != "${local_image_id}" ]] || [[ "${remote_rootfs}" != "${local_rootfs}" ]]; then
  echo "digest-addressed remote image ${repository}@${digest} differs from the locally built config or rootfs" >&2
  exit 1
fi
remote_commit="$(docker run --rm "${repository}@${digest}" --build-commit)"
if [[ "${remote_commit}" != "${FULL_SHA}" ]]; then
  echo "remote image ${repository}@${digest} embeds ${remote_commit}; expected ${FULL_SHA}" >&2
  exit 1
fi

printf '%s\n' "${digest}" > "${RELEASE_DIGEST_DIR}/${CUENV_ARCH}.txt"
