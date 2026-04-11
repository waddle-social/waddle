# Repository Agent Instructions

## XMPP conformance testing
- All XMPP conformance tests are native Rust tests — no Docker, CAAS, or external containers.
- Run all conformance tests: `cargo test -p waddle-xmpp`
- Run protocol conformance suite: `cargo test -p waddle-xmpp --test protocol_conformance`
- Run a specific XEP test: `cargo test -p waddle-xmpp --test xep0199_ping`
- Run all integration tests: `cargo test -p waddle-xmpp --test '*'`
- Every implemented XEP has either:
  - Inline unit tests in `src/xep/xepNNNN.rs` (parsing, building, validation)
  - Integration tests in `tests/xepNNNN_*.rs` (server behavior, IQ handling, MUC)
  - Or both.
- The `protocol_conformance` test replaces the old Docker-based CAAS compliance suite.

## Graceful restart (Ecdysis)

The server implements [Cloudflare's Ecdysis pattern](https://blog.cloudflare.com/ecdysis-rust-graceful-restarts/) for zero-downtime restarts.

### Signal conventions
- `SIGTERM` — Graceful shutdown: stop accepting, drain in-flight connections (30s timeout), exit.
- `SIGQUIT` — Graceful restart: new process starts, old process drains and exits.
- `systemctl reload waddle` sends SIGQUIT (graceful restart).
- `systemctl stop waddle` sends SIGTERM (graceful shutdown).

### Fd inheritance
- On restart, the parent process passes listening sockets to the child via `LISTEN_FDS` / `LISTEN_FD_NAMES` env vars.
- On cold start (no `LISTEN_FDS`), listeners are bound fresh.
- The crate `waddle-ecdysis` handles all fd passing, signal handling, and drain coordination.
- **Unix-only**: `waddle-ecdysis` will not compile on non-Unix platforms.

### State loss on restart
In-memory state is **not** transferred across restarts:
- MUC room presence and rosters
- ISR token store (XEP-0397)
- Stream Management sessions (XEP-0198)
- Connection registry
- PubSub/PEP storage

Connected XMPP clients receive a clean stream close (`</stream:stream>`) during drain and reconnect via XEP-0198 stream resumption. This is acceptable for the current deployment model.

### Configuration
- `WADDLE_DRAIN_TIMEOUT_SECS` — Drain timeout in seconds (default: 30).
