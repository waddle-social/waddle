#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server_root="$(cd "${script_dir}/.." && pwd)"
repo_root="$(cd "${server_root}/.." && pwd)"
gitops_template_dir="${repo_root}/infrastructure/waddle.cloud/gitops/waddle-server"
placeholder_digest="sha256:0000000000000000000000000000000000000000000000000000000000000000"

: "${FULL_SHA:?missing FULL_SHA}"
: "${CUENV_ARCH:?missing CUENV_ARCH}"
: "${GITHUB_REF_NAME:?missing GITHUB_REF_NAME}"
: "${RELEASE_DIGEST_DIR:?missing RELEASE_DIGEST_DIR}"

digest="$(< "${RELEASE_DIGEST_DIR}/${CUENV_ARCH}.txt")"
chart_version="$(< "${RELEASE_DIGEST_DIR}/waddle-server-chart-version.txt")"
chart_digest="$(< "${RELEASE_DIGEST_DIR}/waddle-server-chart-digest.txt")"
modules_yaml="${RELEASE_DIGEST_DIR}/extensions-modules.yaml"

for published_digest in "${digest}" "${chart_digest}"; do
  if [[ ! "${published_digest}" =~ ^sha256:[a-f0-9]{64}$ ]] || [[ "${published_digest}" == "${placeholder_digest}" ]]; then
    echo "invalid immutable release digest: ${published_digest}" >&2
    exit 1
  fi
done
test -s "${modules_yaml}"

extension_digest() {
  local name="$1"
  local value
  value="$(yq -r ".[] | select(.name == \"${name}\") | .digest" "${modules_yaml}")"
  if [[ ! "${value}" =~ ^sha256:[a-f0-9]{64}$ ]] || [[ "${value}" == "${placeholder_digest}" ]]; then
    echo "invalid immutable extension digest for ${name}: ${value}" >&2
    exit 1
  fi
  printf '%s\n' "${value}"
}

link_board_digest="$(extension_digest link-board)"
ai_chatbot_digest="$(extension_digest ai-chatbot)"
decision_polls_digest="$(extension_digest decision-polls)"
github_digest="$(extension_digest github)"
stargate_quotes_digest="$(extension_digest stargate-quotes)"

work_dir="$(mktemp -d)"
cleanup() {
  rm -rf -- "${work_dir}"
}
trap cleanup EXIT

release_dir="${work_dir}/release"
mkdir -p -- "${release_dir}"
cp -R "${gitops_template_dir}/." "${release_dir}/"
helm_release="${release_dir}/helmrelease.yaml"
chart_source="${release_dir}/chart-ocirepository.yaml"

FULL_SHA="${FULL_SHA}" IMAGE_DIGEST="${digest}" \
  yq -i \
    '.spec.values.image.tag = "sha-" + strenv(FULL_SHA) |
     .spec.values.image.digest = strenv(IMAGE_DIGEST) |
     .spec.values.telemetry.commit = strenv(FULL_SHA) |
     .spec.values.extensions.enabled = true' \
    "${helm_release}"
yq -e ".spec.values.image.tag == \"sha-${FULL_SHA}\"" "${helm_release}" >/dev/null
yq -e ".spec.values.image.digest == \"${digest}\"" "${helm_release}" >/dev/null
yq -e ".spec.values.telemetry.commit == \"${FULL_SHA}\"" "${helm_release}" >/dev/null
yq -i ".spec.values.extensions.modules = load(\"${modules_yaml}\")" "${helm_release}"
yq -e '.spec.values.extensions.enabled == true' "${helm_release}" >/dev/null
yq -e '.spec.chartRef.kind == "OCIRepository" and .spec.chartRef.name == "waddle-server-chart" and .spec.chartRef.namespace == "waddle" and (.spec.chart == null)' "${helm_release}" >/dev/null

CHART_DIGEST="${chart_digest}" yq -i \
  '.spec.ref = {"digest": strenv(CHART_DIGEST)}' \
  "${chart_source}"
yq -e ".spec.ref.digest == \"${chart_digest}\" and (.spec.ref.tag == null) and (.spec.ref.semver == null)" "${chart_source}" >/dev/null
yq -e '.kind == "OCIRepository" and .metadata.name == "waddle-server-chart" and .metadata.namespace == "waddle"' "${chart_source}" >/dev/null
yq -e '.spec.url == "oci://ghcr.io/waddle-social/waddle/charts/waddle-server"' "${chart_source}" >/dev/null
yq -e '.spec.layerSelector.mediaType == "application/vnd.cncf.helm.chart.content.v1.tar+gzip" and .spec.layerSelector.operation == "copy"' "${chart_source}" >/dev/null

if grep -R "${placeholder_digest}" "${release_dir}"; then
  echo "refusing to publish GitOps with all-zero digest placeholders" >&2
  exit 1
fi
if grep -R "github-enricher" "${release_dir}"; then
  echo "refusing to publish GitOps with github-enricher" >&2
  exit 1
fi

gitops_values="${work_dir}/values.yaml"
gitops_render="${work_dir}/rendered.yaml"
gitops_kustomize="${work_dir}/kustomized.yaml"
yq -o=yaml '.spec.values' "${helm_release}" > "${gitops_values}"
(
  cd "${server_root}"
  cue vet . "${gitops_values}" -d '#PublishedValues' \
    -t serverImageDigest="${digest}" \
    -t linkBoardDigest="${link_board_digest}" \
    -t aiChatbotDigest="${ai_chatbot_digest}" \
    -t decisionPollsDigest="${decision_polls_digest}" \
    -t githubDigest="${github_digest}" \
    -t stargateQuotesDigest="${stargate_quotes_digest}"
)
helm template waddle-server "${server_root}/charts/waddle-server" \
  --namespace waddle \
  -f "${gitops_values}" > "${gitops_render}"
kubectl kustomize "${release_dir}" > "${gitops_kustomize}"

rendered_image="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | .image' "${gitops_render}")"
case "${rendered_image}" in
  ghcr.io/waddle-social/waddle@"${digest}") ;;
  *) echo "published GitOps render must pin the server image digest ${digest}, got: ${rendered_image}" >&2; exit 1 ;;
esac

rendered_git_sha="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | (.env // [])[] | select(.name == "WADDLE_GIT_SHA") | .value' "${gitops_render}")"
case "${rendered_git_sha}" in
  "${FULL_SHA}") ;;
  *) echo "published GitOps render must set WADDLE_GIT_SHA=${FULL_SHA}, got: ${rendered_git_sha:-<missing>}" >&2; exit 1 ;;
esac

yq -e "select(.kind == \"OCIRepository\" and .metadata.name == \"waddle-server-chart\") | (.spec.ref.digest == \"${chart_digest}\" and (.spec.ref.tag == null))" "${gitops_kustomize}" >/dev/null
yq -e 'select(.kind == "HelmRelease" and .metadata.name == "waddle-server") | (.spec.chartRef.kind == "OCIRepository" and .spec.chartRef.name == "waddle-server-chart")' "${gitops_kustomize}" >/dev/null

chart_ref="ghcr.io/waddle-social/waddle/charts/waddle-server"
current_chart_digest="$(oras resolve "${chart_ref}:${chart_version}")"
if [[ "${current_chart_digest}" != "${chart_digest}" ]]; then
  echo "chart tag ${chart_version} changed from ${chart_digest} to ${current_chart_digest} during publication" >&2
  exit 1
fi

for extension_and_digest in \
  "link-board=${link_board_digest}" \
  "ai-chatbot=${ai_chatbot_digest}" \
  "decision-polls=${decision_polls_digest}" \
  "github=${github_digest}" \
  "stargate-quotes=${stargate_quotes_digest}"; do
  extension_name="${extension_and_digest%%=*}"
  expected_extension_digest="${extension_and_digest#*=}"
  extension_ref="ghcr.io/waddle-social/waddle/extensions/${extension_name}:sha-${FULL_SHA}"
  current_extension_digest="$(oras resolve "${extension_ref}")"
  if [[ "${current_extension_digest}" != "${expected_extension_digest}" ]]; then
    echo "extension locator ${extension_ref} changed from ${expected_extension_digest} to ${current_extension_digest} during publication" >&2
    exit 1
  fi
done

image_ref="ghcr.io/waddle-social/waddle:sha-${FULL_SHA}"
image_manifest="$(docker buildx imagetools inspect "${image_ref}" --format '{{json .Manifest}}')"
current_image_digest="$(printf '%s\n' "${image_manifest}" | yq -r '.digest')"
if [[ "${current_image_digest}" != "${digest}" ]]; then
  echo "image locator ${image_ref} changed from ${digest} to ${current_image_digest} during publication" >&2
  exit 1
fi

gitops_repository="ghcr.io/waddle-social/waddle/gitops-waddle-server"
commit_gitops_ref="${gitops_repository}:sha-${FULL_SHA}"
gitops_exists=false
gitops_resolve_log="${work_dir}/gitops-resolve.log"
if gitops_digest="$(oras resolve "${commit_gitops_ref}" 2>"${gitops_resolve_log}")"; then
  gitops_exists=true
elif ! grep -Eiq 'not found|manifest unknown|manifest_unknown|404' "${gitops_resolve_log}"; then
  cat "${gitops_resolve_log}" >&2
  echo "could not determine whether ${commit_gitops_ref} already exists" >&2
  exit 1
fi

if [[ "${gitops_exists}" == true ]]; then
  if [[ ! "${gitops_digest}" =~ ^sha256:[a-f0-9]{64}$ ]] || [[ "${gitops_digest}" == "${placeholder_digest}" ]]; then
    echo "invalid existing GitOps digest: ${gitops_digest}" >&2
    exit 1
  fi
  remote_preflight="${work_dir}/remote-preflight"
  flux pull artifact "oci://${gitops_repository}@${gitops_digest}" --output "${remote_preflight}"
  if ! diff -qr "${release_dir}" "${remote_preflight}" >/dev/null; then
    echo "full-SHA GitOps locator ${commit_gitops_ref} already exists with different content" >&2
    exit 1
  fi
else
  gitops_push_result="${work_dir}/gitops-push.json"
  flux push artifact "oci://${commit_gitops_ref}" \
    --path="${release_dir}" \
    --source="$(git -C "${repo_root}" config --get remote.origin.url)" \
    --revision="${GITHUB_REF_NAME}@sha1:${FULL_SHA}" \
    --reproducible \
    --output json > "${gitops_push_result}"
  pushed_gitops_digest="$(yq -r '.digest' "${gitops_push_result}")"
  gitops_digest="$(oras resolve "${commit_gitops_ref}")"
  if [[ "${pushed_gitops_digest}" != "${gitops_digest}" ]]; then
    echo "pushed GitOps digest ${pushed_gitops_digest:-<missing>} does not match ${commit_gitops_ref} digest ${gitops_digest}" >&2
    exit 1
  fi
fi

if [[ ! "${gitops_digest}" =~ ^sha256:[a-f0-9]{64}$ ]] || [[ "${gitops_digest}" == "${placeholder_digest}" ]]; then
  echo "invalid immutable GitOps digest: ${gitops_digest}" >&2
  exit 1
fi
remote_verified="${work_dir}/remote-verified"
flux pull artifact "oci://${gitops_repository}@${gitops_digest}" --output "${remote_verified}"
if ! diff -qr "${release_dir}" "${remote_verified}" >/dev/null; then
  echo "digest-addressed GitOps artifact ${gitops_digest} does not match the validated release directory" >&2
  exit 1
fi
printf '%s\n' "${gitops_digest}" > "${RELEASE_DIGEST_DIR}/waddle-server-gitops-digest.txt"

# The cluster's bootstrap-owned OCIRepository currently discovers releases via
# `latest`. Promote only the exact content-addressed artifact after every
# content-addressed digest and pulled byte has been verified. See the release review:
# moving the live source itself to ref.digest needs a separate ownership
# migration because this workflow has no bootstrap/cluster writer.
oras cp "${gitops_repository}@${gitops_digest}" "${gitops_repository}:latest"
latest_gitops_digest="$(oras resolve "${gitops_repository}:latest")"
if [[ "${latest_gitops_digest}" != "${gitops_digest}" ]]; then
  echo "GitOps latest promotion resolved to ${latest_gitops_digest}; expected ${gitops_digest}" >&2
  exit 1
fi
