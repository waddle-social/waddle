#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
staging_root="${repo_root}/target/switchable-baseline-inputs"

: "${WADDLE_CAPABILITY_SERVER_COMMIT:?set WADDLE_CAPABILITY_SERVER_COMMIT}"
: "${WADDLE_BASELINE_WEB_COMMIT:?set WADDLE_BASELINE_WEB_COMMIT}"
: "${WADDLE_CAPABILITY_WINDOW_START:?set WADDLE_CAPABILITY_WINDOW_START}"
: "${WADDLE_CAPABILITY_WINDOW_END:?set WADDLE_CAPABILITY_WINDOW_END}"
: "${WADDLE_BASELINE_CAPTURED_AT:?set WADDLE_BASELINE_CAPTURED_AT}"
: "${WADDLE_CAPABILITY_JOB:?set WADDLE_CAPABILITY_JOB}"
: "${WADDLE_CAPABILITY_ENVIRONMENT:?set WADDLE_CAPABILITY_ENVIRONMENT}"
: "${WADDLE_CAPABILITY_CLUSTER:?set WADDLE_CAPABILITY_CLUSTER}"
: "${WADDLE_CAPABILITY_NAMESPACE:?set WADDLE_CAPABILITY_NAMESPACE}"
: "${WADDLE_CAPABILITY_EXPECTED_REPLICAS:?set WADDLE_CAPABILITY_EXPECTED_REPLICAS}"

bun "${repo_root}/scripts/finalize-switchable-baseline.ts" all \
  --live-disco "${staging_root}/capability/live-disco-export.json" \
  --prometheus "${staging_root}/prometheus/telemetry-baseline.json" \
  --faro-browser-auth-bootstrap "${staging_root}/faro/browser-auth-bootstrap.json" \
  --faro-browser-message-ack-latency "${staging_root}/faro/browser-message-ack-latency.json" \
  --faro-browser-session-lifecycle "${staging_root}/faro/browser-session-lifecycle.json" \
  --faro-browser-reconnect-duration "${staging_root}/faro/browser-reconnect-duration.json" \
  --collection-subject "${staging_root}/attestation/live-collection-subject.json" \
  --attestation-bundle "${staging_root}/attestation/live-collection.sigstore.json" \
  --server-commit "${WADDLE_CAPABILITY_SERVER_COMMIT}" \
  --web-commit "${WADDLE_BASELINE_WEB_COMMIT}" \
  --start "${WADDLE_CAPABILITY_WINDOW_START}" \
  --end "${WADDLE_CAPABILITY_WINDOW_END}" \
  --captured-at "${WADDLE_BASELINE_CAPTURED_AT}" \
  --job "${WADDLE_CAPABILITY_JOB}" \
  --deployment-environment "${WADDLE_CAPABILITY_ENVIRONMENT}" \
  --cluster "${WADDLE_CAPABILITY_CLUSTER}" \
  --namespace "${WADDLE_CAPABILITY_NAMESPACE}" \
  --expected-replicas "${WADDLE_CAPABILITY_EXPECTED_REPLICAS}" \
  --identity-metric waddle_build_info \
  --target-signal-id server-deployment-identity-targets \
  --identity-lookback-seconds 3600
