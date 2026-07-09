#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
server_root="$(cd "${script_dir}/.." && pwd)"
repo_root="$(cd "${server_root}/.." && pwd)"

: "${WADDLE_CAPABILITY_ENDPOINT:?set WADDLE_CAPABILITY_ENDPOINT}"
: "${WADDLE_CAPABILITY_ACCOUNT_JID:?set WADDLE_CAPABILITY_ACCOUNT_JID}"
: "${WADDLE_CAPABILITY_REPRESENTATIVE_MUC_ROOM:?set WADDLE_CAPABILITY_REPRESENTATIVE_MUC_ROOM}"
: "${WADDLE_CAPABILITY_ACCESS_TOKEN:?set WADDLE_CAPABILITY_ACCESS_TOKEN}"
: "${WADDLE_CAPABILITY_XMPP_DOMAIN:?set WADDLE_CAPABILITY_XMPP_DOMAIN}"
: "${WADDLE_CAPABILITY_MUC_DOMAIN:?set WADDLE_CAPABILITY_MUC_DOMAIN}"
: "${WADDLE_CAPABILITY_SPACES_DOMAIN:?set WADDLE_CAPABILITY_SPACES_DOMAIN}"
: "${WADDLE_CAPABILITY_SERVER_COMMIT:?set WADDLE_CAPABILITY_SERVER_COMMIT}"
: "${WADDLE_CAPABILITY_WINDOW_START:?set WADDLE_CAPABILITY_WINDOW_START}"
: "${WADDLE_CAPABILITY_WINDOW_END:?set WADDLE_CAPABILITY_WINDOW_END}"
: "${WADDLE_CAPABILITY_JOB:?set WADDLE_CAPABILITY_JOB}"
: "${WADDLE_CAPABILITY_ENVIRONMENT:?set WADDLE_CAPABILITY_ENVIRONMENT}"
: "${WADDLE_CAPABILITY_CLUSTER:?set WADDLE_CAPABILITY_CLUSTER}"
: "${WADDLE_CAPABILITY_NAMESPACE:?set WADDLE_CAPABILITY_NAMESPACE}"
: "${WADDLE_CAPABILITY_EXPECTED_REPLICAS:?set WADDLE_CAPABILITY_EXPECTED_REPLICAS}"

collector_args=(
  --endpoint "${WADDLE_CAPABILITY_ENDPOINT}"
  --xmpp-domain "${WADDLE_CAPABILITY_XMPP_DOMAIN}"
  --muc-domain "${WADDLE_CAPABILITY_MUC_DOMAIN}"
  --spaces-domain "${WADDLE_CAPABILITY_SPACES_DOMAIN}"
  --account-env WADDLE_CAPABILITY_ACCOUNT_JID
  --representative-muc-room-env WADDLE_CAPABILITY_REPRESENTATIVE_MUC_ROOM
  --access-token-env WADDLE_CAPABILITY_ACCESS_TOKEN
  --calls-configured
  --server-commit "${WADDLE_CAPABILITY_SERVER_COMMIT}"
  --window-start "${WADDLE_CAPABILITY_WINDOW_START}"
  --window-end "${WADDLE_CAPABILITY_WINDOW_END}"
  --job "${WADDLE_CAPABILITY_JOB}"
  --environment "${WADDLE_CAPABILITY_ENVIRONMENT}"
  --cluster "${WADDLE_CAPABILITY_CLUSTER}"
  --namespace "${WADDLE_CAPABILITY_NAMESPACE}"
  --expected-replicas "${WADDLE_CAPABILITY_EXPECTED_REPLICAS}"
  --target-contract "${server_root}/disco-target-contract.json"
  --output "${repo_root}/target/switchable-baseline-inputs/capability/live-disco-export.json"
)

if [[ -n "${WADDLE_CAPABILITY_ORIGIN:-}" ]]; then
  collector_args+=(--origin "${WADDLE_CAPABILITY_ORIGIN}")
fi

cargo run \
  --manifest-path "${server_root}/Cargo.toml" \
  -p waddle-xmpp-client \
  --bin waddle-capability-collector \
  -- \
  "${collector_args[@]}"
