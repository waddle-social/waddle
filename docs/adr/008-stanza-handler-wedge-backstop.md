# ADR-008: Frame-Level Stanza-Handler Wedge Backstop

## Status

Accepted

## Date

2026-05-30

## Context

Issue #757 documented a recurring production incident: per-connection
IQ-handling tasks wedge. New XMPP-over-WebSocket connections complete
SASL + resource bind, then stall before any further IQ activity — the
web client shows an empty channel list because topology discovery never
gets a response. A `kubectl rollout restart` clears it; it recurs after
~3.5 hours.

The per-connection frame loop awaits `handle_xmpp_frame` **inline** with
no isolation:

```text
server/crates/waddle-server/src/server/routes/websocket/connection.rs:71
    let mut responses = handle_xmpp_frame(&text, &domain, &state, &mut conn).await;
```

Any IQ handler that internally `.await`s a long-lived actor will, if that
actor wedges, freeze the entire connection's frame-processing loop.
Subsequent frames buffer at the WebSocket layer; from the outside the
client looks stuck after bind.

The #757 fix series:

- **Fix 1 (#805, merged):** short-circuit MUC `disco#info` for non-MUC
  targets, so component disco no longer couples to `RoomRegistryActor`
  health. This removed the *specific observed* wedge trigger.
- **Fix 2 (#806):** promote disco#info dispatch traces `debug! → info!`.
- **Fix 3 (#807):** instrument `RoomRegistryActor` (mailbox depth, ask
  latency, fail-fast reply timeout).
- **Fix 4 (#808, this ADR):** the *structural* risk remains — "one slow
  handler stalls the whole connection" for **any** future wedge source
  (a different actor, a DB lock, a blocking call), not just the one #805
  fixed. #808 addresses that residual, source-agnostic risk.

Issue #808 was filed "design-first" because the obvious approach —
`tokio::spawn` per IQ — introduces stanza-ordering hazards. This ADR
records the design decision and the reasons the spawn approach was
rejected.

## Decision

Wrap the per-connection **stanza dispatch** in a bounded timeout that
acts as a coarse wedge backstop. Processing remains strictly serial; no
single stanza can freeze the connection indefinitely.

### 1. Mechanism: bounded latency, not concurrency

Wrap the post-parse stanza dispatch (the `InboundFrame::Stanza` arm in
`frame.rs`) in `tokio::time::timeout(STANZA_HANDLER_WEDGE_TIMEOUT, …)`.
Because dispatch stays serial, **RFC 6120 §10.1 in-order processing is
preserved for free** — there is no reordering window.

We explicitly **reject** `tokio::spawn`-per-IQ isolation:

- RFC 6120 §10.1 requires in-order stanza processing per stream. Naive
  spawn-per-frame reorders responses relative to requests.
- The frame loop owns `conn: WsConnState` single-threaded; registration,
  XEP-0198 outbound recording, and the `<r/>` ack cadence
  (`connection.rs:86–183`) all mutate it after dispatch returns. True
  per-IQ spawn would require moving `conn` behind a shared lock or a
  per-connection actor — a large, race-prone rewrite.
- After #805 (trigger removed) and #807 (actor fails fast), the only
  residual is "an *unknown* handler hangs *indefinitely*." A coarse
  backstop addresses that directly; the ownership rewrite is
  over-engineering against a now-mitigated risk.

### 2. Threshold: wedge backstop, not latency SLO

`const STANZA_HANDLER_WEDGE_TIMEOUT: Duration = Duration::from_secs(15)`.

The backstop must sit **above the slowest legitimate handler's own
internal budget**, so it fires only on a genuine indefinite hang. The
slowest legitimate internal budget in-tree is profile/avatar enrichment:
`profile::fetch::TOTAL_TIMEOUT = 10s` (`profile/fetch.rs:89`). 15s clears
that with margin and stays well under the wasm client's own IQ timeout
(~30s), so the synthesized error is actionable before the client gives
up.

This is deliberately a **coarse backstop**, distinct from #807's tight
per-`.ask()` `reply_timeout` (the *fast*, cancellation-safe fail-path for
the known actor wedge). 15s is the *catch-all* for unknown future
sources.

### 3. Conformance on elapse (RFC 6120 §8.2.3 / §8.3)

The timeout wraps **stanza dispatch inside `frame.rs`** — not the opaque
`handle_xmpp_frame` at `connection.rs:71` — specifically so the parsed
IQ's `id`/`from`/`to` are in scope to build a conformant reply.

- **IQ `get`/`set`:** RFC 6120 §8.2.3 requires exactly one `result` or
  `error` response. On elapse we synthesize
  `<iq type='error'><resource-constraint/></iq>`. `resource-constraint`
  (`xmpp_parsers::stanza_error::DefinedCondition::ResourceConstraint`)
  carries error type **`wait`** per RFC 6120 §8.3.3 — the honest signal
  for a temporary, retryable server-side condition. Built via the typed
  `errors.rs` constructors + `build_iq_error_xml_typed` (per the
  CLAUDE.md XML and typed-payloads hard rules — no `format!` XML).
  - `id`/`from`/`to` are captured **before** the IQ is moved into
    `handle_iq_with_conn_state` (it takes the IQ by value).
- **Message / Presence:** no response is owed; on elapse we log + emit
  the metric and continue (dropping the timed-out work is conformant).
- The synthesized error is returned in the `Vec<String>` response set, so
  the existing XEP-0198 outbound recording in `connection.rs:148–152`
  (`is_countable_stanza` matches `iq`) records it for replay
  automatically — no special-casing.

### 4. Cancellation safety

`tokio::time::timeout` drops the handler future mid-`.await` on elapse.
This is safe at the 15s backstop range:

- DB writes go through `DbActor.ask(DbExecute{…})`
  (`db/actor.rs:50–61`). The actor runs the `execute`/`commit` to
  completion on its **own** task once the message is dequeued; dropping
  the caller's future cancels only the *reply wait*, not the committed
  statement. The database stays internally consistent.
- Rust futures yield only at `.await` points, so a dropped dispatch
  leaves `conn` structurally valid. The worst case is a multi-statement
  handler dropped between a committed write and its follow-on fanout
  (e.g. `roster_set`: row committed, XEP-0237 push not yet enqueued).
  This only occurs if one of those awaits is itself hung for 15s — the
  very wedge we are surviving — and the conformant `wait` reply tells the
  client to retry; roster versioning heals the gap.

A sub-second latency SLO was rejected precisely because it would drop
in-flight legitimate writes routinely and demand a full
cancellation-safety audit of every handler.

### 5. Observability (all layers, OTEL-native)

The wedge event is the primary diagnostic #757 lacked. On elapse:

- **Log:** `warn!` at the elapse site with stable fields
  `{ stanza_kind, id, from, to, payload_ns, timeout_secs }`. `warn`
  (not `error`) — it is a handled, self-healing condition; surfaces at
  prod's INFO level and rides the existing `OpenTelemetryTracingBridge`
  into OTLP logs (`telemetry.rs:229`).
- **Metric:** a new counter `xmpp.stanza.handler.timeout` with helper
  `record_stanza_handler_timeout(stanza_kind, payload_ns)` in
  `metrics.rs`, following the existing `record_actor_request_timeout`
  pattern. Distinct axis from #807's actor-keyed metrics: this is keyed
  by stanza kind + namespace, so we learn *which handler family* wedged.
  Exports over OTLP via the global meter provider
  (`telemetry.rs:193`) — no new wiring.
- **Trace:** a `#[instrument]` span per stanza dispatch (lean fields
  `stanza_kind`/`id`/`payload_ns`, heavy args skipped). Becomes an OTEL
  span automatically via `tracing_opentelemetry::layer()`
  (`telemetry.rs:223`), parented into the connection trace with the
  browser-propagated `traceparent`. On elapse: a `timeout` span event
  and `otel.status_code = ERROR`. Per-stanza spans also give free
  handler-latency distribution — the SLO visibility #757 lacked.

No changes to `telemetry.rs` and no new dependencies: traces, metrics,
and logs all ride providers already initialized there.

### 6. Scope guard

The timeout wraps **post-parse stanza dispatch only**. Stream framing
(`Open`/`Close`), SASL, and resource binding arms are excluded so the
backstop never interferes with stream setup.

## Relationship to #807

#808's original "land #807 first" gate is **dissolved** by this design.
That gate assumed spawn-based isolation, which would need #807's
mailbox/latency signal to choose what to offload. The chosen
frame-level backstop is **source-agnostic** — it reads no actor state and
needs nothing from #807 to function or be tested. The two are
**complementary, independent** layers that may land in any order:

- **#807** — fast, specific fail-path for the *known* actor wedge
  (per-`.ask()` `reply_timeout` → typed error in ms, no future drop).
- **#808** — coarse, universal backstop for *unknown/future* wedge
  sources (15s, defense-in-depth).

## Testing

Per the XEP custom-test-suite and TDD rules:

- A `#[cfg(test)]` fault-injection sentinel forces a single stanza
  dispatch to `std::future::pending()`, exercising the real
  `timeout(…)` wrapper and the real conformant error-synthesis path.
  Driven under `#[tokio::test(start_paused = true)]` with
  `tokio::time::advance` (precedent: `push/limiter.rs`,
  `connection_registry/tests.rs`) — deterministic, no real 15s wait.
- A paired transparency test asserts a fast handler does **not** trip
  the timeout and its normal response passes through unchanged.
- Conformance assertions: timed-out IQ get/set yields
  `<iq type='error'>` with `resource-constraint` + type `wait` and the
  echoed `id`; timed-out message/presence yields no response frame but
  records the metric.

## Consequences

**Positive**

- An indefinite wedge becomes a ≤15s-bounded, observable, self-healing
  event instead of a connection-wide silent hang requiring a pod
  restart.
- Strictly serial → zero stanza-ordering risk; no `conn` ownership
  rewrite.
- Conformant `resource-constraint`/`wait` replies; clients retry
  correctly.
- Full OTEL traces + metrics + logs for any residual wedge, with no new
  telemetry plumbing.

**Negative / trade-offs**

- A per-stanza span raises baseline trace volume (accepted for the
  in-context diagnostic value; fields kept lean).
- The backstop masks rather than roots out a slow handler; the metric +
  span are how we then find and fix the underlying cause.
- A multi-statement handler dropped mid-sequence can leave self-healing
  stale in-memory state (bounded, rare, retryable — see §4).

## References

- #757 (root incident), #805 (fix 1, merged), #806 (fix 2), #807 (fix 3)
- RFC 6120 §8.2.3 (IQ semantics), §8.3 (stanza errors), §10.1 (in-order
  processing)
- `profile/fetch.rs:89` (`TOTAL_TIMEOUT` floor for the threshold)
- `telemetry.rs` (OTEL trace/metric/log providers reused as-is)
