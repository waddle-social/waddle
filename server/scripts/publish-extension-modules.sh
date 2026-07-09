#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server_root="$(cd "${script_dir}/.." && pwd)"
placeholder_digest="sha256:0000000000000000000000000000000000000000000000000000000000000000"

: "${FULL_SHA:?missing FULL_SHA}"
: "${RELEASE_DIGEST_DIR:?missing RELEASE_DIGEST_DIR}"

rustup target add wasm32-wasip2 >/dev/null 2>&1 || true
extensions=(
  "link-board:link_board"
  "ai-chatbot:ai_chatbot"
  "decision-polls:decision_polls"
  "github:github"
  "stargate-quotes:stargate_quotes"
)
declare -A digests=()
work_dir="$(mktemp -d)"
cleanup() {
  rm -rf -- "${work_dir}"
}
trap cleanup EXIT

for extension_spec in "${extensions[@]}"; do
  IFS=: read -r extension_name crate_name <<< "${extension_spec}"
  wasm_path="${server_root}/target/wasm32-wasip2/release/${crate_name}.wasm"
  extension_ref="ghcr.io/waddle-social/waddle/extensions/${extension_name}:sha-${FULL_SHA}"
  extension_repository="${extension_ref%:*}"

  cargo build \
    --release \
    --locked \
    --target wasm32-wasip2 \
    --target-dir "${server_root}/target" \
    --manifest-path "${server_root}/extensions/${extension_name}/Cargo.toml"
  test -s "${wasm_path}"

  if [[ "${extension_name}" == "ai-chatbot" ]] \
    && grep -aE "AI provider unavailable|WADDLE_AI_PROVIDER|OPENROUTER_API_KEY|OPENAI_API_KEY" "${wasm_path}" >/dev/null; then
    echo "Refusing to publish ai-chatbot WASM with legacy server-provider fallback strings" >&2
    exit 1
  fi

  local_dir="${work_dir}/local-${extension_name}"
  mkdir -p -- "${local_dir}"
  cp -- "${wasm_path}" "${local_dir}/module.wasm"

  resolve_log="${work_dir}/resolve-${extension_name}.log"
  extension_exists=false
  if extension_digest="$(oras resolve "${extension_ref}" 2>"${resolve_log}")"; then
    extension_exists=true
  elif ! grep -Eiq 'not found|manifest unknown|manifest_unknown|404' "${resolve_log}"; then
    cat "${resolve_log}" >&2
    echo "could not determine whether ${extension_ref} already exists" >&2
    exit 1
  fi

  if [[ "${extension_exists}" == false ]]; then
    push_result="${work_dir}/push-${extension_name}.json"
    (
      cd "${local_dir}"
      oras push \
        --artifact-type "application/vnd.waddle.extension.wasm.v1+wasm" \
        "${extension_ref}" \
        "module.wasm:application/wasm" \
        --format json
    ) > "${push_result}"
    pushed_digest="$(yq -r '.digest' "${push_result}")"
    extension_digest="$(oras resolve "${extension_ref}")"
    if [[ "${pushed_digest}" != "${extension_digest}" ]]; then
      echo "pushed ${extension_name} digest ${pushed_digest:-<missing>} does not match remote tag digest ${extension_digest}" >&2
      exit 1
    fi
  fi

  if [[ ! "${extension_digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
    echo "Invalid digest for ${extension_name}: ${extension_digest}" >&2
    exit 1
  fi
  if [[ "${extension_digest}" == "${placeholder_digest}" ]]; then
    echo "Refusing to pin all-zero digest placeholder for ${extension_name}" >&2
    exit 1
  fi

  remote_dir="${work_dir}/remote-${extension_name}"
  mkdir -p -- "${remote_dir}"
  (
    cd "${remote_dir}"
    oras pull "${extension_repository}@${extension_digest}" >/dev/null
  )
  shopt -s nullglob dotglob
  remote_entries=("${remote_dir}"/*)
  shopt -u nullglob dotglob
  if [[ "${#remote_entries[@]}" -ne 1 ]] \
    || [[ "${remote_entries[0]:-}" != "${remote_dir}/module.wasm" ]] \
    || [[ ! -f "${remote_dir}/module.wasm" ]] \
    || [[ -L "${remote_dir}/module.wasm" ]]; then
    echo "digest-addressed ${extension_name} artifact did not contain one regular module.wasm" >&2
    exit 1
  fi
  if ! cmp -s "${local_dir}/module.wasm" "${remote_dir}/module.wasm"; then
    echo "digest-addressed ${extension_name} artifact does not match the locally built WASM" >&2
    exit 1
  fi
  resolved_digest="$(oras resolve "${extension_ref}")"
  if [[ "${resolved_digest}" != "${extension_digest}" ]]; then
    echo "full-SHA extension locator ${extension_ref} moved during verification" >&2
    exit 1
  fi
  digests["${extension_name}"]="${extension_digest}"
done

modules_yaml="${RELEASE_DIGEST_DIR}/extensions-modules.yaml"
modules_tmp="${work_dir}/extensions-modules.yaml"
(
  cd "${server_root}"
  cue export . -e '#PublishedExtensionModules' --out yaml \
    -t linkBoardDigest="${digests[link-board]:?missing link-board digest}" \
    -t aiChatbotDigest="${digests[ai-chatbot]:?missing ai-chatbot digest}" \
    -t decisionPollsDigest="${digests[decision-polls]:?missing decision-polls digest}" \
    -t githubDigest="${digests[github]:?missing github digest}" \
    -t stargateQuotesDigest="${digests[stargate-quotes]:?missing stargate-quotes digest}"
) > "${modules_tmp}"
mv "${modules_tmp}" "${modules_yaml}"
