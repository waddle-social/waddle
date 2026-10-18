# ADR-0019: One client state layer in Rust

## Status

Proposed (2026-10-18). Revised four times after independent architecture
reviews: the first returned 3 blockers and 7 required changes, the second
1 blocker and 6, the third 1 blocker and 4, the fourth 2. All are
addressed below. ADR-0018 is
reserved for the multi-tenancy decision.

## Context

Waddle has three clients and a fourth is planned (Linux, GTK4 +
libadwaita). They share the Rust protocol layer, `waddle-xmpp-client`
(sans-IO `XmppRuntime`, XEP-0198, MAM queries, stanza builders and
parsers). They do not share client state. Each client implements its own:

- connection loop: reconnect backoff, liveness and the ready pipeline;
- outbox;
- timeline store: mutations, paging, trimming and grouping;
- presence store;
- unread, mention, inbox and read-marker state.

The copies are large and disagree:

- **Web** keeps about 14K lines of TypeScript state on top of the wasm
  client (`chat/src/lib/xmpp/client.ts` alone is 3,991 lines).
- **Apple** (`WaddleKit`) and **Android** (`:core:client`) each have their
  own stores, tested with their own fakes (476 and 782 tests). There are
  no shared fixtures.
- Measured divergences at `44ae8a8`:
  - **Reconnect:** three different backoff curves, and web gives up after
    10 attempts.
  - **Liveness:** web pings every 60 s, Android every 180 s, Apple only on
    foreground.
  - **Presence:** Apple ranks Do Not Disturb below Away, against ADR-010.
  - **Mentions:** each client counts and clears them differently.
  - **Outbox:** Android caps it at 50 and drops rejected sends; Apple keeps
    them retryable.
  - **Catch-up:** Apple reloads the visible conversation; Android fetches
    every room plus the top 3 DMs.
  - **Grouping:** Apple uses `<=` 5 minutes, Android `<`.
  - **Resumption:** Apple asks the server to park sessions it never
    resumes (fixed separately); Android turns resumption off.
- **The native path loses events.** The client's events go through a
  256-slot `broadcast` (`waddle-xmpp-client/src/client.rs:173`). The FFI
  dispatcher skips `Lagged` (`waddle-xmpp-client-ffi/src/lib.rs:226`), and
  unmapped events fall into `_ => {}` (`convert.rs:113`). Both platforms
  then buffer callbacks without bound (`XmppEventBridge.kt:15`,
  `FFIEventListener.swift:17`), so there is no backpressure either.

Every behaviour fix has had to be ported by hand. Android missed iOS's MAM
and liveness fixes (`claude/android-recovery-plan.md`). A fourth copy would
make this worse.

## Decision

### Layering

Client state moves into Rust, in two new crates under `server/crates`.

**`waddle-client-session`** is the state machines. It is sans-IO and uses
no tokio *runtime*; `tokio::sync` primitives are allowed, since they work
in wasm.

- It sits directly on `XmppRuntime`, as the wasm driver already does
  (`waddle-xmpp-client-wasm/src/driver.rs:274`), not on `ClientHandle` and
  its broadcast.
- IQ correlation and timeouts are core state, built on the existing
  `RequestTracker` (`request.rs:160`). Today it only covers connect and
  disconnect. Both drivers keep their own `pending_iqs` maps
  (`client.rs:207`, wasm `driver.rs:285-287`), and `IqResult` is emitted
  without resolving the tracker (`runtime/app_stanza.rs:23-29`). The
  tracker gains:
  - per-request deadlines that feed `poll_timeout()`;
  - resolution inside the runtime;
  - reply matching on both id and the expected `from` JID (today it
    matches on id only, `client.rs:581`);
  - `IqCancelled` (`event.rs:86`) for requests abandoned by a disconnect.
- Time is deadline-driven, in the style of quinn-proto:
  - the driver asks `poll_timeout()` for the next deadline;
  - the driver calls `handle_timeout(now)` when it passes.

  This replaces the 250 ms stream-management pulse (`client.rs:255`).
- Time is a core-owned monotonic type, `MonoMillis(u64)`: milliseconds from
  an arbitrary per-process origin, supplied by the driver. `poll_timeout()`
  and `handle_timeout(now)` take it. It is not `std::time::Instant`, which
  panics on `wasm32-unknown-unknown` and is not UniFFI-exportable, and not
  `chrono` wall time, which the stream-management clock uses today
  (`runtime.rs:84`).
- The 5 s host-suspension heuristic (`runtime.rs:242`) goes. The platform
  reports suspend and resume as lifecycle signals.
- Inputs: transport events, the current time, a seeded RNG, lifecycle and
  network signals, and commands.
- Outputs: frames to write, plus store updates.

**`waddle-client-session-tokio`** is the native driver.

- It replaces `DriverTask`; it does not wrap it.
- It owns the socket, timers and the platform adapters (storage,
  credentials).
- It owns the only HTTP in the shared code: the XEP-0363 upload PUT, and
  the RFC 8628 device-flow sign-in, which runs before XMPP exists.
- HTTP uses rustls with `ring`, matching `waddle-xmpp-client` (Android NDK
  cross-builds).
- The Giphy proxy stays a platform-side, out-of-band exception
  (`GifSearch.kt:15-19`). It does not enter the shared core, per
  `AGENTS.md`.

**Where the UniFFI derives live.** The core's public types carry their
UniFFI derives behind a feature (`cfg_attr(feature = "uniffi", …)`). The
wasm build never sees UniFFI, and there are no mirror types. Mirror types
with hand-written conversions are what produced today's lossy
`convert.rs:113 _ => {}`. Where a conversion is unavoidable it is an
exhaustive `match`, with no wildcard arm, enforced by
`clippy::wildcard_enum_match_arm` on the conversion module.

**One socket, one owner.** The roughly 6K lines of FFI verbs that call
`ClientHandle::send_iq` today (`messaging_verbs.rs`, `profile_verbs.rs`,
`room_admin.rs` and others) move under the session as methods on its
stores. `WaddleClient` is retired when the last platform migrates. Until
then, an account is driven by exactly one of the two, never both.

### What the core owns

1. **`AccountManager`:** several accounts at once, keyed by domain and bare
   JID.
2. **`ConnectionController`:**
   - reconnect, liveness and the ready pipeline (policies below);
   - terminal states.

   It honours `server/docs/reconnect-presence-contract.md`: a fresh bind is
   not a continuation; resumption is.
3. **`Outbox`:** queue semantics. Durable storage sits behind a foreign
   trait, and that implementation is responsible for encrypting at rest.
4. **`Timeline`:** one per conversation.
   - mutations: corrections, retractions, reactions, pins, safety scores;
   - MAM paging (pages only, never `MamResult` events);
   - trim with refetch, threads, day separators and grouping (time zone
     injected).
5. **`PresenceStore`** (ADR-010 ranking), `PeopleDirectory`, and
   self-presence with auto-away. Idle input comes from the platform.
6. **`ReadState`:** XEP-0430 inbox, unread, mentions, XEP-0490 cursors,
   XEP-0492 notify modes, and the unread overview.
7. **`PushPolicy`:** which device to register or retire. Push transports
   stay on the platform.

Call state machines are deferred to a later revision. CallKit and Telecom
impose OS-driven timing, and calls would widen the FFI before the stores
are proven.

Platforms keep:
- OS notifications;
- push transports;
- lifecycle and network signals;
- credential storage;
- media SDKs and audio sessions;
- rendering;
- the Giphy proxy.

### Credentials and terminal states

- **Fresh token on every attempt.** The core calls an async foreign trait,
  `CredentialProvider::fresh_token()`, on every connection attempt. A
  static token in the config cannot be refreshed, and "never give up" on a
  stale token is an endless authentication loop.
- **Some failures stop the loop, carefully.**
  - `AuthenticationFailed` becomes terminal only after one forced
    `fresh_token()` retry has also failed.
  - Stream `<conflict/>` is terminal only when it arrives on the current
    transport generation. A reconnect that reuses the resource replaces the
    old stream, and the server then writes `<conflict/>` to the stale one
    (shown by the test at `connection.rs:2629`). That must not end the new
    session.
  - Terminal states are surfaced to the UI, not fed to backoff. Android
    already treats them as terminal (`ConnectionLoop.kt:232`).
- **Tokens never print.** They stay in `AccessToken`, which redacts in
  `Debug` (`waddle-xmpp-client/src/config.rs:177`).

### Observation

The whole session API is UniFFI-exportable. CI fails on a public type that
is not. Linux consumes the same facade as Swift and Kotlin, so the design
cannot drift onto Rust-only shapes such as `Stream`, generics or `usize`.

- **Lists are pulled, not pushed.**
  - Each list store (timeline, conversation list, people) hands out a
    subscription object with `async fn next_batch() -> Vec<…Diff>`.
  - Each item type has its own concrete diff enum: `Reset`, `Append`,
    `Insert`, `Set`, `Remove`, `Truncate`, with `u32` indices.
  - `next_batch` is cancel-safe: the subscription's cursor advances only
    when a batch is actually returned.
  - Each subscription has an explicit `close()` that wakes any pending
    `next_batch`. Kotlin cancellation drops the Rust future (generated
    `waddle_xmpp_client.kt:1647-1661`), but Swift's does not: the
    generated `uniffiRustCallAsync` frees the future only after it
    completes (`waddle_xmpp_client.swift:17911-17925`). Swift calls
    `close()` from the task's cancellation handler.
- **A slow subscriber gets a reset, never a gap.** Each subscription keeps
  a cursor into a bounded diff log. A subscriber that falls behind the
  log's retention gets `Reset` with a full snapshot.
- **Scalar state** (connection state, counts, presence of one contact) is
  pulled the same way with `next_snapshot()`. Intermediate values coalesce.
- **On GTK:** `next_batch` runs on `glib::MainContext::spawn_local`, with
  the tokio runtime on its own thread. Each batch becomes one
  `items_changed` call.
- **Typed values at the FFI.** JIDs cross as `BareJid` and `FullJid`,
  not `String`. Both come from the `jid` crate, so they are declared with
  `uniffi::custom_type!(…, { remote, … })` once, and `use_remote_type!` in
  every other crate that exposes them. FFI records are immutable.

### Behaviour

These resolve the divergences above. The scenario fixtures encode them.

| Concern | Behaviour |
|---|---|
| Reconnect | Never give up, except on the terminal states above. Full-jitter backoff: uniform [1 s, min(60 s, 2ⁿ s)], from the injected RNG. Reset only after the ready pipeline completes **and** the stream stays up for 60 s. |
| Liveness | XEP-0199 ping on foreground, network identity change and send failure. Periodic cadence depends on a platform-injected mode: **foreground** 60 s; **background socket** 180 s; **push** no periodic ping. One 10 s timeout, owned by the core (the FFI's 8 s ping timeout goes). A failed ping retires the transport. |
| Ready pipeline | Self-presence, rooms with queued sends, drain them, topology, remaining joins, drain the rest, inbox, bounded catch-up. |
| Presence ranking | ADR-010: DND > available/chat > away > xa > offline. |
| Mentions | Live mentions are counted with one matcher: the bare JID, broadcast mentions, and the user's own room nick. On a fresh stream, mentions are recomputed from catch-up MAM pages with the same matcher, not reset. XEP-0430 carries no mention count (`inbox.rs:28-41`). |
| Outbox | Capped at 500 per sending account, and entries expire 7 days after they were queued. A full outbox refuses new sends with a visible state; it never evicts. Queued content is frozen: editing a queued message discards it and queues a new one. Origin-ids are deduplicated by the server's ingress alias, keyed on sender bare JID, target and origin-id, for DMs and MUC alike (`ingress/commit.rs:164-213`); the alias lives 8 days (`ingress_substrate/mod.rs:51`), which bounds the expiry. Entries outlive the process, so `MonoMillis` cannot age them: the durable store records the platform's wall-clock enqueue time, and the 1-day margin under the alias lifetime absorbs client clock error. An entry whose enqueue time lies in the future (the clock moved backwards) is treated as expired, since its real age is unknown. Same-origin resend is automatic only for sends whose outcome is unknown, such as a connection lost before the ack. A committed denial owns its origin-id (`commit.rs:159-161`), so a user retry of a **rejected** send mints a new origin-id; reusing it would replay the recorded rejection. A changed body under the same origin-id is rejected as `AliasConflict` (`commit.rs:206-209`), which frozen content prevents. |
| Catch-up | Visible conversation first, then rooms with unread or mentions, then the rest. MAM queries 4 in flight; inbox queries stay single-flight, as the wasm driver already does (`driver.rs:866`). |
| Grouping | Same author within 5 minutes inclusive. Break on day change, unread divider, tombstone and call rows. |

**Dependency: a distinguishable quota bounce.** A message bounced because
the recipient's offline queue is full is already archived
(`offline_delivery.rs:126-134`), so it must not be offered for retry. On the
wire it is a plain `cancel`/`service-unavailable` (`offline_delivery.rs:731-737`),
the same condition as the nonexistent-account bounce
(`route_to_connection_plan.rs:101-110`) and the bot PM refusal
(`muc_direct.rs:172-178`), neither of which is archived. The server must add
an application-specific condition (`urn:waddle:*`) to the quota bounce before
the client classifies it. Until then it is treated as an ordinary failed send.

### Resumption

- **Now:** native clients pass `resumable = false`, because the FFI never
  resumes. Apple's default was fixed in #1987. Web keeps its working
  resume (`chat/src/lib/xmpp/resume-persistence.ts`).
- **When Rust resumption lands:** a native client asks for a **short**
  resumption window (`max`) as a matter of course, not as an option. The
  OS can suspend an app before any clean shutdown completes, so a long
  window would park the session for minutes. The server treats messages
  queued to a parked session as delivered and skips push until the window
  ends (`delivery_immediate.rs:227-232`; the default window is 300 s,
  `waddle-xmpp-client/src/config.rs:204`).
- **Backgrounding while a push transport is registered.** The client
  closes cleanly, in this order:
  1. It sends a final `<a h='…'/>`. Frames are handled in order, and the
     ack is applied before `<close/>` reaches cleanup (`frame.rs:375-413`,
     `stream_management.rs:532`; during a write pause, acks are applied
     ahead of deferred frames, `batch_write.rs:1016-1047`). The clean-close
     path (`cleanup.rs:1489-1546`) does not promote the server's unacked
     queue; only the resumable detach paths do (`cleanup.rs:923`, `1210`).
     A non-resumable drop, today's native configuration, skips them too
     (`cleanup.rs:866-868`).

     A window remains: stanzas the server writes after the final ack, and
     anything still in the connection's outbound queue, are neither
     acknowledged nor promoted. They are archived, so MAM catch-up on the
     next foreground shows them, but they get no push. That window is
     accepted.
  2. It posts local notifications for anything received during shutdown.
  3. It sends `<close/>`. The server moves the connection to `Closing`,
     which is never detached for resume (`frame.rs:448-454`,
     `cleanup.rs:866-867`). The test
     `intentional_close_wins_inflight_force_resume_without_persisting_a_snapshot`
     (`waddle-server/src/server/routes/websocket/tests/stream_management.rs:13684`)
     enables resumption, sends a real `<close/>` and asserts that no
     session is kept for resume. Later messages are routed as for an offline user, to
     offline storage and push. A server test asserting that route must land
     with this rule.

  The consequence is accepted. The clean close broadcasts unavailable
  presence and leaves every room (`cleanup.rs:1513-1527`), so each
  background is a visible departure and each foreground a full rejoin.
  Before this ships, a server test must show that MUC mention push still
  reaches a member who is not currently an occupant.
- **Resume failure.** XEP-0198 `<failed/>` falls back to a fresh bind
  (`runtime/sm.rs:185-218`). After that, the core rejoins rooms, resets
  live-only state and runs catch-up, as the reconnect-presence contract
  requires. Server restarts do not preserve stream-management sessions
  (`server/AGENTS.md`), so this path is routine, not exceptional.

### Migration and coexistence

- **One owner per account.** Exactly one implementation owns an account's
  connection and outbox at a time.
- **Keep the resource.** The persisted resource is reused
  (`ServerSettings.resource`, `resourceSuffix`), so a switch cannot leave
  ghost occupants.
- **Existing queues** are either imported or drained before switching:
  - Apple's versioned JSON envelope (`OutboxFile.swift:4-37`) gets an
    importer.
  - Android's owner-scoped `SessionPrefs` rows (`OutboundQueue.kt:22-37`)
    get an importer.
  - If an importer is not ready, the app drains the queue on the old
    implementation before switching.
- **Order:**
  1. Linux consumes first.
  2. Android reviews the API first: its `ConnectionLoop` is closest to the
     target, and its 782 tests are the best regression oracle. Its
     migration follows Linux.
  3. Apple next.
  4. Web optionally, through a wasm driver, once native has proven the
     stores.

### Verification

- **Fixtures are data, never logic.**
  - They are serde types in Rust with `deny_unknown_fields` and an exported
    JSON Schema.
  - Inputs: `advance(ms)`; inbound frames (XML, parsed once at the fixture
    boundary); lifecycle and network signals; commands; and an RNG seed.
  - Outputs: frames written, and projected store snapshots or diff batches.
  - No conditionals and no loops, so a fixture cannot become a second
    implementation.
- **Expected outputs are golden files,** approved from the Rust run.
- **Kotlin and Swift run fixtures only through the FFI.** That tests the
  bindings and the platform adapters, not a parallel store. Nobody writes
  fixture interpreters against the pre-migration native stores.
- **Coverage:** half-open sockets, network flips, slow IQs, bursts during
  catch-up, process kills with queued sends, and resume failure after a
  server restart.

## Consequences

- One behaviour to fix, one suite to extend, one place to read. Platform
  layers shrink to UI, notifications, media and adapters.
- The client crates gain a deadline-driven core, a credential trait, a
  storage trait and a narrow HTTP client in the native driver.
- The FFI surface is rebuilt around sessions and stores. Binding
  regeneration and the drift check
  (`server/scripts/check-xmpp-client-ffi-bindings.sh`) become part of
  every session-crate change. `WaddleClient` and its single listener are
  removed at the end of migration.
- Web keeps its TypeScript state until a wasm driver exists. The fixtures
  make it checkable but do not remove it.

## Alternatives considered

- **Keep per-platform state and add a Linux copy.** Cheapest now. It makes
  four copies and repeats the drift that produced the Android recovery
  plan.
- **Build the session on `ClientHandle`.** Least change, but it inherits
  the broadcast's silent loss, and awaited IQ futures rule out sans-IO.
- **Push callbacks with platform-side buffers.** This is today's model. It
  has no backpressure and can lose or reorder state. It was rejected for
  pull-based diffs.
- **Fixture interpreters per platform.** Three interpreters against
  diverging models. Rejected for FFI-only fixture runs.
