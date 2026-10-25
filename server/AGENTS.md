# Repository Agent Instructions

## Rust quality and protocol boundaries

- For Rust API, import and module-layout changes, follow
  [the Rust style conventions](../docs/rust-style.md).
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

- Create metrics only through `waddle_xmpp::counter_add!`, `waddle_xmpp::histogram_record!`, and `waddle_xmpp::gauge_record!` in `crates/waddle-xmpp/src/telemetry/`; do not add a standalone registration API.
- Alert-worthy counters are zero-registered at startup through the emitting helper's own `counter_add!` call site, driven by `telemetry::reliability::register_reliability_counters()` from `waddle-server::telemetry::init`. Counters whose alerts trigger above zero on a healthy pod belong in that path; counters whose alerts trigger only when they remain zero stay unregistered. See the telemetry module docs.
- Current-state gauges publish a complete successful snapshot, including zero values, through the emitting helper's `gauge_record!` calls. Record snapshot freshness last; a query failure preserves the previous values and freshness.
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
- In-process server state for `tests/xepNNNN_*.rs` comes from `waddle_server::test_support` (feature `test-support`; suites that need it carry `#![cfg(feature = "test-support")]` and run in the `--all-features` lanes and the `waddle-server-xmpp-server-tests` lane, which enables the feature explicitly; never enabled in the release image).
- The active C2S transport is WebSocket only; do not add TCP C2S or S2S harness tests.

## Listener inheritance and graceful shutdown

- `SIGTERM` and `SIGQUIT` stop accepting, drain connections and exit. A process
  supervisor or deployment controller starts the replacement; the server does
  not fork or re-exec itself.
- An external launcher can transfer sockets through `LISTEN_FDS` and
  `LISTEN_FD_NAMES`. Capture their exclusive ownership once in synchronous
  startup, before creating the runtime or opening unrelated descriptors, and
  register them with Tokio after entering the runtime. Cold starts bind fresh
  sockets. `waddle-ecdysis` is Unix-only.
- Process-local actors and registries are rebuilt on replacement. Determine
  resumption and delivery recovery from their durable storage and ownership
  fences; a process restart alone establishes neither loss nor completion.
- `WADDLE_DRAIN_TIMEOUT_SECS` configures the drain timeout (default: 30).
- For startup secret handling and descriptor safety changes, read
  [the startup safety policy](../docs/planning/startup-safety-2020-2024.md).
