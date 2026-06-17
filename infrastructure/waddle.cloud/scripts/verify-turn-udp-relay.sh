#!/usr/bin/env bash
# Verify the self-hosted LiveKit embedded-TURN UDP relay is reachable
# end-to-end and cross-check the call-media telemetry, per issue #1000.
#
# This is the HITL/ops step: the chart and gitops config are validated by
# `go test ./...`, but external reachability and the TCP-relay share can
# only be confirmed against the running environment.
#
# Usage:
#   ./scripts/verify-turn-udp-relay.sh [host:port]
#
# Defaults to turn.waddle.social:30478 (production NodePort for the
# TURN/UDP listener; kept in lockstep with gitops/livekit-sfu/helmrelease.yaml).
#
# Prerequisites:
#   - Go toolchain (runs the `verify-turn-udp` STUN probe)
#   - Outbound UDP to the target host:port from where you run this
#
# A reachable result proves a UDP datagram traversed the NodePort and node
# firewall to the TURN server and a reply returned, so LiveKit can hand
# clients a usable `udp` relay candidate instead of forcing TCP/443.
set -euo pipefail

ADDR="${1:-turn.waddle.social:30478}"
MODULE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> Probing TURN/UDP relay reachability at ${ADDR}"
if (cd "${MODULE_DIR}" && go run . verify-turn-udp --addr "${ADDR}"); then
  reachable=1
else
  reachable=0
fi

cat <<EOF

==> Telemetry cross-check (call-media relay-rate, established by #996/#998)

The probe above confirms the UDP listener is reachable from THIS host. To
confirm real calls are not silently stuck on the TCP relay, inspect the
call-media telemetry emitted on the Faro beacon:

  - Field: succeeded ICE candidate-pair transport (udp vs tcp) and type
    (host / srflx / relay), per published/subscribed call.
  - Acceptance: the share of calls whose succeeded pair is a *TCP relay*
    must be at or below the pre-change baseline. A non-trivial TCP-relay
    share with this probe reporting "reachable" points at a client-network
    or DNS/external-IP issue rather than the cluster data plane.

If the probe reports NOT reachable, check, in order:
  1. NodePort exposure   — kubectl -n livekit get svc livekit-sfu-turn-udp
  2. External IP / DNS    — turn.waddle.social resolves to the node ingress IP
  3. Node firewall / LB   — 30478/UDP open inbound at the cloud/edge layer
  4. relay port range     — livekit.turn.relay_range_start/end reachable for media
  5. probe assumption     — the probe expects the embedded TURN server to
                            answer an unauthenticated STUN Binding request;
                            a LiveKit/pion-turn upgrade could change that and
                            yield a false negative against a healthy relay.

EOF

if [ "${reachable}" -eq 1 ]; then
  echo "==> TURN/UDP relay reachable from this host."
else
  echo "==> TURN/UDP relay NOT reachable from this host; see checklist above." >&2
  exit 1
fi
