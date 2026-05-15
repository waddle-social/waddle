#!/usr/bin/env bash
#
# State-inventory scraper for the Waddle server canary.
#
# Pulls `/debug/state-inventory` every INTERVAL seconds and writes
# both a raw JSON-lines log and a flat TSV that's easy to import
# into a spreadsheet or chart against the existing `/metrics`
# Prometheus scrape. Run this on a workstation with `kubectl
# port-forward` open to the canary pod (or against any reachable
# server URL).
#
# The server only mounts the debug route when WADDLE_DEBUG_STATE_TOKEN
# is set, and every request must carry that token in the
# X-Waddle-Debug-Token header — see
# server/crates/waddle-server/src/server/state_inventory_route.rs.
#
# Usage:
#   WADDLE_DEBUG_STATE_TOKEN=$(op read 'op://...') \
#       scripts/state-inventory-scrape.sh \
#         --url http://localhost:8080/debug/state-inventory \
#         --interval 30 \
#         --out /tmp/waddle-state-inventory
#
# Outputs (rotated by date):
#   /tmp/waddle-state-inventory.jsonl   — one line per scrape
#   /tmp/waddle-state-inventory.tsv     — TSV with header for charting

set -euo pipefail

URL=""
INTERVAL=30
OUT_PREFIX="/tmp/waddle-state-inventory"
DURATION_SECS=0  # 0 = run forever

while [[ $# -gt 0 ]]; do
    case "$1" in
        --url) URL="$2"; shift 2 ;;
        --interval) INTERVAL="$2"; shift 2 ;;
        --out) OUT_PREFIX="$2"; shift 2 ;;
        --duration) DURATION_SECS="$2"; shift 2 ;;
        -h|--help)
            grep '^#' "$0" | head -40 | sed 's/^# \?//'
            exit 0
            ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

if [[ -z "$URL" ]]; then
    echo "--url is required" >&2
    exit 2
fi
if [[ -z "${WADDLE_DEBUG_STATE_TOKEN:-}" ]]; then
    echo "WADDLE_DEBUG_STATE_TOKEN must be set in the environment" >&2
    exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required" >&2
    exit 2
fi

JSONL="${OUT_PREFIX}.jsonl"
TSV="${OUT_PREFIX}.tsv"
mkdir -p "$(dirname "$JSONL")"

if [[ ! -s "$TSV" ]]; then
    cat > "$TSV" <<'EOF'
ts	pending_auth	device_auth	xmpp_auth_codes	dynamic_oidc_clients	avatar_source_locks	profile_publish_in_flight	provider_dispatch_in_flight	sm_live_sessions	resumable_sessions	caps_cache	caps_pending	full_jid_conns	pending_subs	presence_states	last_activity	rooms_total	rooms_dormant
EOF
fi

start_ts=$(date +%s)
while true; do
    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    body=$(curl -fsS -H "X-Waddle-Debug-Token: $WADDLE_DEBUG_STATE_TOKEN" "$URL" || true)
    if [[ -n "$body" ]]; then
        printf '{"ts":"%s","inventory":%s}\n' "$now" "$body" >> "$JSONL"
        echo -e "$now\t$(jq -r '
            [
              .auth.pending_auth,
              .auth.device_auth,
              .auth.xmpp_auth_codes,
              .auth.dynamic_oidc_clients,
              .profile.avatar_source_locks,
              .profile.profile_publish_tracker_in_flight,
              .profile.provider_dispatch_tasks_in_flight,
              (.sessions.sm_live_sessions // 0),
              .sessions.resumable_sessions,
              .caps.caps_cache,
              .caps.pending_resolutions,
              .connections.full_jid_connections,
              .connections.pending_subscription_stanzas,
              .connections.presence_states,
              .connections.last_activity,
              .rooms.total,
              .rooms.dormant
            ] | @tsv
        ' <<<"$body")" >> "$TSV"
    else
        printf '{"ts":"%s","error":"empty body or non-2xx"}\n' "$now" >> "$JSONL"
    fi
    if [[ "$DURATION_SECS" -gt 0 ]]; then
        elapsed=$(( $(date +%s) - start_ts ))
        if (( elapsed >= DURATION_SECS )); then
            break
        fi
    fi
    sleep "$INTERVAL"
done
