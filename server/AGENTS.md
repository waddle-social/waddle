# Repository Agent Instructions

## Rust quality and protocol boundaries

- Rust toolchain changes follow [`docs/rust-toolchain-bump.md`](../docs/rust-toolchain-bump.md).
- Local and CI Clippy checks MUST use `-D warnings`. Do not add `#[allow(...)]`, `#![allow(...)]`, or Clippy-specific suppressions unless necessary and justified in the PR description. Prefer removing dead code, exercising it, or narrowing visibility.
- Never construct XMPP/XML with `format!`, string concatenation, or `println!`. Build payloads with Rust structs/builders (`xmpp_parsers`, `minidom::Element`, etc.) and serialize them.
- Model protocol data with typed Rust values at every boundary. Do not use `String`, `&str`, or `Vec<u8>` to carry structured protocol data in events, traits, actor messages, dispatcher entries, callbacks, storage writes, routing effects, or public return types.
  - Use `crate::connection::Stanza` / `xmpp_parsers::{Iq, Message, Presence}` for stanzas, `minidom::Element` for arbitrary XML, and `jid::{Jid, BareJid, FullJid}` for JIDs.
  - Use dedicated enums or constants from `xep::*` modules for namespaces and XEP identifiers; do not add ad-hoc string literals at call sites.
  - Serialize to `String` or `Vec<u8>` only at the I/O boundary. Parse raw frames and storage rows into typed values exactly once, as early as possible, then drop the untyped form. Use typed errors (`thiserror` enums or typed stanza-error structs); `String` is acceptable only for human-facing log messages emitted through the `Log` event.
  - A new string field on an event, message, trait method, or public struct must not carry structured protocol data; use a typed value or enum variant.
- Every implemented XEP, including advertised compatibility or profile support, MUST have a dedicated Rust custom test suite. Changes that add or expand XEP behavior must update that suite in the same PR. If an advertised feature lacks testable behavior, implement and test it or remove the advertisement.

## Rust workflow

- Format Rust changes with `cargo fmt` before committing. Do not introduce `unwrap()` in production code; handle or propagate errors explicitly.

## Ordered relay wire compatibility
- Any wire-format change to `RemoteStanzaEnvelope` or ordered relay reply types (including `OrderedRelayReply`, `OrderedRelayAck`, `OrderedRelayNack`, and their contained fields) MUST bump the version suffix in the `waddle.clustering.relay.deliver_ordered.vN` remote message ID in `clustering/relay.rs`. Reusing an ID can make mixed-version peers fail during deserialization after a delivery has committed, poisoning the shared ordered channel.

## Telemetry

- Create metrics only through `waddle_xmpp::counter_add!` and `waddle_xmpp::histogram_record!` in `crates/waddle-xmpp/src/telemetry/`; do not add a standalone registration API.
- Alert-worthy counters are zero-registered at startup through the emitting helper's own `counter_add!` call site, driven by `telemetry::reliability::register_reliability_counters()` from `waddle-server::telemetry::init`. Counters whose alerts trigger above zero on a healthy pod belong in that path; counters whose alerts trigger only when they remain zero stay unregistered. See the telemetry module docs.
- The Prometheus text renderer in `crates/waddle-xmpp/src/prometheus.rs` is frozen; do not add metric families there.
- Metric names use dot.case, units use UCUM on the instrument, and attributes come only from `telemetry/attributes.rs`'s allowlist. Never use JIDs, room JIDs, stream IDs, or message IDs as metric attributes; use spans or logs and follow the telemetry module's cardinality budget.
- Test exported metric samples through `telemetry::test_support`'s in-memory reader, not instrument internals.

## XMPP conformance testing
- All XMPP conformance tests are native Rust tests — no Docker, CAAS, or external containers.
- Run the server workspace test suite, including XMPP/XEP and advertised-feature coverage: `cargo nextest run --workspace --all-targets --locked --profile ci` (from `server/`).
- Run a specific XEP test: `cargo nextest run -p waddle-xmpp --test xep0172_pep_nick`
- Run `waddle-xmpp` integration tests: `cargo nextest run -p waddle-xmpp --tests`
- Run doctests (nextest cannot): `cargo test --doc --workspace --all-features`
- Dedicated tests may be inline unit tests in `crates/waddle-xmpp/src/xep/xepNNNN.rs` or `crates/waddle-xmpp-core/src/xepNNNN.rs` for parsing, building, or validation, integration tests in `crates/waddle-xmpp/tests/xepNNNN_*.rs`, WebSocket tests in `crates/waddle-server/tests/xepNNNN_*.rs`, or a combination. Each suite must assert the behavior of the XEP it covers.
- The active C2S transport is WebSocket only; do not add TCP C2S or S2S harness tests.

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
- Stream Management sessions (XEP-0198)
- Connection registry
- PubSub/PEP storage

Connected XMPP clients receive a clean stream close (`</stream:stream>`) during drain and reconnect via XEP-0198 stream resumption. This is acceptable for the current deployment model.

### Configuration
- `WADDLE_DRAIN_TIMEOUT_SECS` — Drain timeout in seconds (default: 30).
