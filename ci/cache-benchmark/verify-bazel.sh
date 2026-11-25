#!/usr/bin/env bash
set -euo pipefail

phase="${1:?expected seed or restore}"
: "${CUENV_REMOTE_CACHE:?Namespace did not provision a remote cache endpoint}"
case "$phase" in
  seed)
    [[ "$GITHUB_EVENT_NAME" == push && "$GITHUB_REF" == refs/heads/main ]]
    [[ "$CUENV_REMOTE_CACHE_UPLOAD" == true && "$CUENV_CACHE" == write ]]
    ;;
  restore)
    [[ "$CUENV_REMOTE_CACHE_UPLOAD" == false && "$CUENV_CACHE" == read ]]
    [[ "${EXPECTED_ACTION_DIGEST:?missing seed digest}" =~ ^[0-9a-f]{64}/[0-9]+$ ]]
    ;;
  *) exit 2 ;;
esac

CUENV_CACHE_DIR="$(mktemp -d "${RUNNER_TEMP:?}/crd-cache.XXXXXX")"
export CUENV_CACHE_DIR
events="$(mktemp "$RUNNER_TEMP/crd-events.XXXXXX")"
trap 'rm -f "$events"' EXIT
if ! cuenv --json ci --all --pipeline cacheVerification --path operator > "$events"; then
  jq -Rc 'fromjson? | objects | .category | select(.type == "Task" or .type == "Output") | .data' "$events" | tail -c 8000 >&2
  exit 1
fi

# CLI output can also contain unstructured command output. Inspect only the
# task executor's typed events, including the reason for a skipped upload.
jq -Rrc 'fromjson? | objects | .category | select(.type == "Cache" or .type == "Task")
  | .data | select(.data.task == "ciCheckCrdDrift" or .data.name == "ciCheckCrdDrift")
  | select(.event | test("Cache|RemoteUpload"))' "$events"
digest="$(jq -Rr --arg phase "$phase" '
  fromjson? | objects | .category |
  if $phase == "seed" then
    select(.type == "Cache") | .data | select(.event == "RemoteUploaded" and .data.task == "ciCheckCrdDrift") | .data.digest
  else
    select(.type == "Task") | .data | select(.event == "CacheHit" and .data.name == "ciCheckCrdDrift") |
    .data.cache_key | ltrimstr("sha256:")
  end' "$events")"
if [[ ! "$digest" =~ ^[0-9a-f]{64}/[0-9]+$ ]]; then
  echo "::error::Missing or ambiguous CRD drift cache evidence for $phase."
  exit 1
fi
if [[ "$phase" == restore && "$digest" != "$EXPECTED_ACTION_DIGEST" ]]; then
  echo "::error::Restored CRD drift action $digest differs from seed $EXPECTED_ACTION_DIGEST."
  exit 1
fi
echo "digest=$digest" >> "$GITHUB_OUTPUT"
echo "Operator CRD drift check $phase verified for \`$digest\`." >> "$GITHUB_STEP_SUMMARY"
