# TODO.md

## Test runtime snapshot (2026-04-11)

- `cargo test -p waddle-xmpp` (full suite): **~10s** (~1,600 tests)
- `cargo test -p waddle-xmpp --lib` (unit only): **~1.2s** (~1,315 tests)
- `cargo test -p waddle-xmpp --test protocol_conformance`: **~5s** (20 tests)

All tests are native Rust — no Docker, CAAS, or external dependencies required.

## Remaining work

- [ ] Expand XEP-0292 (vCard4) PEP publish/retrieve integration tests
- [ ] Add XEP-0402 (Bookmarks) PEP integration tests
- [ ] Add XEP-0115 (Entity Capabilities) integration tests
- [ ] Add XEP-0047 (In-Band Bytestreams) IQ session integration tests
