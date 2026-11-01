#!/usr/bin/env bash
# Authenticated TURN qualification against an explicitly controlled UDP echo peer.
# This verifies Allocate, CreatePermission, ChannelBind and payload echo, not
# merely an unauthenticated STUN Binding response. Supports UDP and TLS:443.
#
# Usage (credentials must already be present in the environment):
#   TURN_USERNAME / TURN_PASSWORD: short-lived coturn REST credentials
#   ./scripts/verify-turn-udp-relay.sh --addr <test-turn>:3478 --peer <echo-ip>:3479
#   ./scripts/verify-turn-udp-relay.sh --transport tls --addr <test-turn>:443 \
#     --peer <echo-ip>:3479 [--ca <qualification-ca.pem>]
#
# For a self-contained pinned-coturn qualification:
#   bash internal/turnprobe/testdata/qualify-coturn.sh
#
# Run this against an isolated qualification server with a deliberately allowed
# echo peer. The production policy allows SFU media only, and the SFU is not a
# raw echo service. Do not relax production peer restrictions to run this probe.
set -euo pipefail

: "${TURN_USERNAME:?provide short-lived TURN_USERNAME through the environment}"
: "${TURN_PASSWORD:?provide TURN_PASSWORD through the environment}"
module_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
(cd "${module_dir}" && go run . verify-turn-udp "$@")

cat <<'EOF'

Authenticated controlled-peer qualification passed. Production acceptance also
requires a real web and Android call from a restricted network:
  - Force and confirm a selected relay ICE candidate on UDP and TLS:443.
  - Verify bidirectional media, not just successful credential issuance.
  - Exercise a coturn rollout while an allocation is active.
  - Confirm denied peer destinations/ports remain denied by the live policies.
  - Correlate client relay telemetry with the actual selected candidate pair.

A local echo qualification does not prove production routing, Cilium enforcement,
Kubernetes draining or restricted-network/device media behavior.
EOF
