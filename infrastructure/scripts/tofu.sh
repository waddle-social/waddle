#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOFU_DIR="${SCRIPT_DIR}/../tofu"

export OP_ACCOUNT="waddle-social.1password.eu"

export AWS_ACCESS_KEY_ID="$(op read 'op://waddle-infra/Scaleway S3/access_key')"
export AWS_SECRET_ACCESS_KEY="$(op read 'op://waddle-infra/Scaleway S3/secret_key')"
if [[ -z "${TF_VAR_proxmox_api_token:-}" ]]; then
  export TF_VAR_proxmox_api_token="$(op read 'op://waddle-infra/Login/einvbbtkrcn232jry4d66ye2cq')"
fi

cd "${TOFU_DIR}"
exec tofu "$@"
