#!/usr/bin/env bash
set -euo pipefail

: "${FULL_SHA:?missing FULL_SHA}"
: "${CUENV_ARCH:?missing CUENV_ARCH}"
: "${GITHUB_REF_TYPE:?missing GITHUB_REF_TYPE}"
: "${GITHUB_REF_NAME:?missing GITHUB_REF_NAME}"
: "${RELEASE_DIGEST_DIR:?missing RELEASE_DIGEST_DIR}"

repository="ghcr.io/waddle-social/waddle"
digest="$(< "${RELEASE_DIGEST_DIR}/${CUENV_ARCH}.txt")"
if [[ ! "${digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
  echo "invalid immutable image digest: ${digest}" >&2
  exit 1
fi

remote_digest() {
  local reference="$1"
  local manifest
  manifest="$(docker buildx imagetools inspect "${reference}" --format '{{json .Manifest}}')"
  printf '%s\n' "${manifest}" | yq -r '.digest'
}

commit_ref="${repository}:sha-${FULL_SHA}"
commit_ref_digest="$(remote_digest "${commit_ref}")"
if [[ "${commit_ref_digest}" != "${digest}" ]]; then
  echo "full-SHA image locator ${commit_ref} moved from ${digest} to ${commit_ref_digest}" >&2
  exit 1
fi

tag_args=()
if [[ "${GITHUB_REF_TYPE}" == "tag" ]]; then
  version="${GITHUB_REF_NAME}"
  tag_args+=("-t" "${repository}:${version}")
  if [[ "${version}" =~ ^v?([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    major="${BASH_REMATCH[1]}"
    minor="${BASH_REMATCH[2]}"
    patch="${BASH_REMATCH[3]}"
    tag_args+=("-t" "${repository}:${major}.${minor}.${patch}")
    tag_args+=("-t" "${repository}:${major}.${minor}")
    tag_args+=("-t" "${repository}:${major}")
  fi
else
  tag_args+=("-t" "${repository}:main")
fi

docker buildx imagetools create --prefer-index=false "${tag_args[@]}" "${repository}@${digest}"
for ((tag_index = 1; tag_index < ${#tag_args[@]}; tag_index += 2)); do
  promoted_ref="${tag_args[${tag_index}]}"
  promoted_digest="$(remote_digest "${promoted_ref}")"
  if [[ "${promoted_digest}" != "${digest}" ]]; then
    echo "promoted image locator ${promoted_ref} resolves to ${promoted_digest}; expected ${digest}" >&2
    exit 1
  fi
done

final_commit_ref_digest="$(remote_digest "${commit_ref}")"
if [[ "${final_commit_ref_digest}" != "${digest}" ]]; then
  echo "full-SHA image locator ${commit_ref} moved during convenience-tag promotion" >&2
  exit 1
fi
