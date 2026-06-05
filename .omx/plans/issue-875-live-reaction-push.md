# Issue 875 TDD Plan

- Server WS integration: subscribed member receives summary-node fan-out when another member reacts.
- Server implementation: summary republish flows through normal PubSub fan-out.
- Rust client/WASM: generic typed pubsub-event parser and callback union, preserving unknown payloads opaquely.
- Chat client: route summary events by node URI into the story reaction store.
- Connect bootstrap: fetch summary-node catch-up, then subscribe once service-wide.
- Verification: focused tests per slice, then chat test/lint and server clippy/tests.
