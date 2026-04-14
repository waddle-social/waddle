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
