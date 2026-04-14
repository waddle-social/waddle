# Call Stack Operator Runbook

Call control is now **XMPP-native**.

## Scope

- The server no longer exposes HTTP media orchestration routes:
  - `/v1/media/backend`
  - `/v1/media/sessions`
  - `/v1/media/calls*`
  - bootstrap/leave helpers
- Call lifecycle and media signaling are controlled through XMPP signaling paths (for example, Jingle/call XEP flows handled by `waddle-xmpp`).

## Operational guidance

- Validate call behavior through XMPP integration tests and client flows, not HTTP call-control probes.
- Keep `/health`, `/ready`, `/api/v1/health`, and `/metrics` for service readiness and observability checks.

## Metrics to monitor

- `waddle_call_failures_total{reason=...}` should remain low; spikes indicate transport/routing/session failures.
- `waddle_call_duration_seconds{outcome=...}` should show expected session duration distributions.
- Correlate call metric spikes with XMPP disconnect/reconnect events in logs.

## Muji troubleshooting flow

1. Confirm room occupants are present in MUC and discovery advertises Muji/Jingle features.
2. Verify call invitation and join/accept signaling stanzas are exchanged in the room timeline.
3. Confirm Jingle session-initiate/session-accept and transport-info IQ routing reaches intended occupants.
4. If media never becomes active, inspect ICE/TURN configuration and peer connectivity at the SFU boundary.
