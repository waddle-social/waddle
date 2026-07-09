#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server_root="$(cd "${script_dir}/.." && pwd)"
: "${RELEASE_DIGEST_DIR:?missing RELEASE_DIGEST_DIR}"

chart_ref="ghcr.io/waddle-social/waddle/charts/waddle-server"
chart_url="oci://${chart_ref}"
work_dir="$(mktemp -d)"
chart_preflight_log="${work_dir}/preflight.log"
helm_push_log="${work_dir}/push.log"

cleanup() {
  rm -rf -- "${work_dir}"
}
trap cleanup EXIT

mkdir -p \
  "${work_dir}/package" \
  "${work_dir}/local" \
  "${work_dir}/remote-preflight" \
  "${work_dir}/remote-verified"

chart_package="$(helm package "${server_root}/charts/waddle-server" -d "${work_dir}/package" | awk '{print $NF}')"
chart_version="$(helm show chart "${chart_package}" | awk '$1 == "version:" {print $2; exit}')"
if [[ -z "${chart_version}" ]]; then
  echo "packaged Helm chart has no version" >&2
  exit 1
fi
tar -xzf "${chart_package}" -C "${work_dir}/local"

chart_exists=false
if helm pull "${chart_url}" --version "${chart_version}" --untar --untardir "${work_dir}/remote-preflight" >"${chart_preflight_log}" 2>&1; then
  chart_exists=true
elif ! grep -Eiq 'not found|manifest unknown|manifest_unknown|404' "${chart_preflight_log}"; then
  cat "${chart_preflight_log}" >&2
  echo "could not determine whether chart version ${chart_version} already exists" >&2
  exit 1
fi

if [[ "${chart_exists}" == true ]]; then
  if ! diff -qr "${work_dir}/local/waddle-server" "${work_dir}/remote-preflight/waddle-server" >/dev/null; then
    echo "Chart version ${chart_version} already exists with different content; bump charts/waddle-server/Chart.yaml version" >&2
    exit 1
  fi
  echo "Chart version ${chart_version} already exists with identical content, skipping push" >&2
elif ! helm push "${chart_package}" oci://ghcr.io/waddle-social/waddle/charts 2>&1 | tee "${helm_push_log}" >&2; then
  # A concurrent publisher may have won after the absence check. The digest
  # pull and content comparison below remain mandatory after a conflict.
  if ! grep -Eiq 'already exists|409|conflict' "${helm_push_log}"; then
    exit 1
  fi
fi

chart_digest="$(oras resolve "${chart_ref}:${chart_version}")"
if [[ ! "${chart_digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
  echo "invalid OCI manifest digest for chart ${chart_version}: ${chart_digest}" >&2
  exit 1
fi

helm pull "${chart_url}@${chart_digest}" --untar --untardir "${work_dir}/remote-verified" >/dev/null
if ! diff -qr "${work_dir}/local/waddle-server" "${work_dir}/remote-verified/waddle-server" >/dev/null; then
  echo "remote chart ${chart_version}@${chart_digest} does not match the packaged chart" >&2
  exit 1
fi

printf '%s\n' "${chart_version}" > "${RELEASE_DIGEST_DIR}/waddle-server-chart-version.txt"
printf '%s\n' "${chart_digest}" > "${RELEASE_DIGEST_DIR}/waddle-server-chart-digest.txt"
printf '%s\n' "${chart_digest}" > "${RELEASE_DIGEST_DIR}/waddle-server-chart-${chart_version}.txt"
