# #2140 dark SASL2 authentication core

## Objective

Add a testable XEP-0388 authentication path around the existing SCRAM-SHA-256
and OAUTHBEARER mechanisms. The implementation is a dark protocol core; it does
not change production feature negotiation or activate `/ws` behavior.

## Boundaries

- Keep server SASL2 advertisement and frame dispatch behind an explicit
  `test-support` capability. The production build neither advertises nor
  accepts SASL2, regardless of `WADDLE_XMPP_PUBLIC_WEBSOCKET_URL`.
- Preserve the current SASL1 path and its mechanism behavior.
- Reuse existing authentication and session principal construction. Do not add
  credentials, tokens, or authorization snapshots to stream-management state.
- Reuse the shared client runtime's existing SASL2 support and add only the
  runtime/WASM changes needed for this handshake.
- Exclude XEP-0198 inline resume, XEP-0386 Bind 2, production advertisement or
  activation, and CI/workflow changes.

## Verification

- Add dedicated XEP-0388 Rust tests over the loopback WebSocket for the
  test-only positive handshake, malformed and unsupported requests,
  authentication failure, production-default refusal, and SASL1 compatibility.
- Run focused server, shared-client, and WASM checks, formatting, and strict
  scoped Clippy.
- The loopback fixture proves application WebSocket negotiation and state
  behavior only. It does not prove TLS transport or TLS 0-RTT behavior; the
  production transport boundary remains dark and needs a separate review.
