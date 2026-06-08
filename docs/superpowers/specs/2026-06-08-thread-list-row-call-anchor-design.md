# Thread-list-row call anchor (issue #919, AC #4) — design

Date: 2026-06-08
Branch / PR: `issue-919-live-call-anchor` / #926
Closes the remaining acceptance item of **#919** ("Call Chat 3/7: live state + rich
anchor card + banner dedup (MUC)"): the **thread-list row** must reflect the same
live/ended call state as the banner and thread-panel header, and share one Join
action.

## Background

PR #926 already added the shared live-state composable (`readCallAnchorCardState` /
`useCallAnchorCardState`), the `CallAnchorCard`, and wired the channel timeline
anchor (`MessageCard.vue`) and the thread-panel header (`ThreadPanel.vue`). It
deliberately deferred the **global Threads view row** (`ThreadsListRow.vue`)
because that row is fed by the server `urn:waddle:threads:0` query
(`WasmThreadEntry`), which carries no call-thread metadata, and the live MUC
detector is keyed only by room JID (it cannot tell a row "you are *the* call
thread").

## Goal

A `ThreadsListRow` that represents a MUC call-thread anchor renders the
`CallAnchorCard`:
- **Live** (room currently has an active call): pulse, media icon, participant
  count, Join, "N messages in call chat".
- **Ended**: muted "Call ended · <duration>", no Join — including for channels the
  client has not loaded into memory.

All three surfaces (banner, thread-list row, thread-panel header) read the one
shared composable and invoke the one shared `joinChannelCall` path — no divergence.

## Constraints (from CLAUDE.md)

- **No new XMPP wire data.** The `urn:waddle:call-thread:0` anchor marker and the
  XEP-0422 `<call-thread-ended>` fastening already exist on the wire (slices
  #916/#918). We only surface what is already there through the *internal*
  `urn:waddle:threads:0` query response shape (a `urn:waddle:*` namespace, free to
  extend; no official XEP conflict).
- **Typed payloads.** New `InboxEntry` / `ThreadEntry` fields are typed
  (`CallThreadKind`, `CallThreadMedia`, `CallThreadDuration`), not strings.
  Serialization to `String` happens only at the SQL/WASM boundary.
- **XEP custom test-suite rule.** Threads-query + call-thread behavior changes ship
  with Rust tests in the same PR.
- `clippy -D warnings` (server), `knip` clean (chat), no channel-threading
  regressions.
- Breaking changes by default; no production data — schema columns added directly,
  no back-compat shims.

## Architecture / data flow

```
MUC call anchor message (urn:waddle:call-thread:0 + <thread> + origin-id)
  └─ MucInboxHandler.thread_projection  ──capture kind+media──┐
                                                              ▼
                                          GroupchatThreadProjection (+ call_thread)
                                                              ▼
                                ProjectGroupchatInbox event ─► interpret.rs
                                                              ▼
                          InboxEntry (+ call_thread_kind, call_thread_media)  ──► inbox_entries
call ends (last participant) → maybe_broadcast_call_thread_ended
  └─ ActiveCallThread (now carries thread_id) ──► inbox.mark_call_thread_ended(room, thread_id, ended, duration)
                                                              ▼
                          inbox_entries.call_ended_at / call_duration  (UPDATE across all users for that thread)
                                                              ▼
list_all_threads → build_thread_entry → ThreadEntry (+ call_thread*) → WaddleThreadEntry → WasmThreadEntry
                                                              ▼
ThreadsListRow.vue: wasmThreadEntryToAnchorMessage(entry) → useCallAnchorCardState(msg, channel, count) → CallAnchorCard
                                                              │ live state from useRoomHasActiveCall(entry.channel)
                                                              └ Join → shared joinChannelCall path
```

## Server changes (Rust)

1. **`InboxEntry`** (`waddle-xmpp/src/inbox/mod.rs`) — add typed optional fields and
   builder methods:
   - `call_thread_kind: Option<CallThreadKind>`
   - `call_thread_media: Option<CallThreadMedia>`
   - `call_ended_at: Option<DateTime<Utc>>` (or unix secs at the storage boundary)
   - `call_duration: Option<CallThreadDuration>`

2. **Anchor capture** (`waddle-xmpp/src/protocol/room/inbox.rs`) — extend
   `GroupchatThreadProjection` with `call_thread_kind` + `call_thread_media`, parsed
   from the anchor child of the message in `thread_projection`. Thread root markers
   only (the anchor is the thread root). Propagate through `ProjectGroupchatInbox`
   into the `InboxEntry` built in `interpret.rs`.

3. **Ended correlation** (`websocket/state.rs`, `handlers/presence/muc_update.rs`,
   `routes/muc_muji_clear.rs`):
   - Add `thread_id: String` to `ActiveCallThread`; capture it from the anchor's
     `<thread>` when inserting into `protocol.call_threads`.
   - In `maybe_broadcast_call_thread_ended`, after removing the `ActiveCallThread`
     (now with `thread_id`), call a new
     `InboxStorage::mark_call_thread_ended(partner=room, thread_id, ended, duration)`
     that issues a single
     `UPDATE inbox_entries SET call_ended_at=?, call_duration=? WHERE partner_jid=? AND thread_id=?`
     — updating every user's row for that thread without enumerating recipients
     (the inbox table is shared, keyed by `(user_jid, partner_jid, thread_id)`).

4. **Storage** (`waddle-server/src/inbox/schema.rs`, `inbox.rs`, `inbox/codec.rs`):
   - Add nullable columns: `call_thread_kind TEXT`, `call_thread_media TEXT`,
     `call_ended_at BIGINT`, `call_duration TEXT` (ISO-8601 duration string).
   - Extend `SELECT_COLS`, the UPSERT (carry kind+media via `COALESCE` so a later
     reply does not wipe the anchor's metadata), row decode, and add the
     `mark_call_thread_ended` UPDATE.

5. **Threads query** (`waddle-server/src/threads/query.rs`, `storage.rs`) —
   `ThreadEntry` gains the four call-thread fields; `build_thread_entry` copies them
   from `InboxEntry`.

6. **WASM bridge** (`waddle-xmpp-client-wasm/src/types.rs`, `client_account.rs`) —
   `WaddleThreadEntry` gains `call_thread: Option<{ kind, media }>` and
   `call_thread_ended: Option<{ ended, duration }>` (camelCase via serde),
   `skip_serializing_if = None`.

## Contract / frontend (TypeScript)

7. **`WasmThreadEntry`** (`chat/src/lib/xmpp/wasm-types.ts`) — add optional
   `callThread?: { kind: "muc"; media: ("audio"|"video")[] }` and
   `callThreadEnded?: { ended: string; duration: string }`.

8. **Adapter** (`chat/src/lib/call-thread-anchor.ts`) — add a pure
   `wasmThreadEntryToAnchorMessage(entry): Pick<TimelineMessage,"body"|"author"|"callThread"|"threadId"> | null`
   returning `null` when `entry.callThread` is absent or `kind !== "muc"`, else a
   message-shaped object with `callThread: { kind, media, ended?, duration? }` and
   `threadId: entry.thread_id`. This feeds the *existing*
   `useCallAnchorCardState(message, roomJid, messageCount)` unchanged.

9. **`ThreadsListRow.vue`** — when the adapter yields a call anchor:
   - render `CallAnchorCard` with `useCallAnchorCardState(() => msg, () => entry.channel, () => entry.reply_count)`;
   - `@join` → emit a shared join intent the panel routes to `joinChannelCall`
     (same path as the banner); `@open-thread` → existing open behavior.
   - otherwise render the current row markup unchanged.

   (Scope: the global Threads view row is the deferred AC surface. The channel
   sidebar list / `TopicsPanel` rows are a low-value follow-up — the thread-panel
   header and timeline anchor already cover the active-conversation surface — and
   are out of scope here.)

10. **Visible ended duration** (`buildCallAnchorCardState` in
    `call-thread-anchor.ts`) — the ended `title` is currently the literal
    `"Call ended"`; the duration only reaches `ariaLabel`. Change the ended title to
    `Call ended · <formatted duration>` when `callThread.ended && duration`, so the
    AC's visible "Call ended · <duration>" holds across all three surfaces (timeline
    anchor, thread-panel header, global row). Reuse the existing
    `formatCallThreadDuration`. No behavior change when duration is absent.

## Testing

- **Rust**
  - inbox storage: upsert persists kind+media; `mark_call_thread_ended` sets
    ended+duration for all users' rows of a thread; reply upsert does not clear
    kind+media (`waddle-server/src/inbox/tests.rs`).
  - threads query: `ThreadEntry` carries the call-thread fields end-to-end
    (`tests/waddle_threads_query_ws.rs`).
  - call-thread anchor projection: anchor message yields a projection with
    kind+media; non-anchor reply does not (inbox handler test / call-thread suite).
- **TypeScript**
  - `wasmThreadEntryToAnchorMessage`: maps a MUC call entry; returns `null` for
    non-call / non-muc / missing metadata.
  - `ThreadsListRow.vue` SSR render: live entry → pulse + Join + "N messages";
    ended entry (`callThreadEnded` set, no active call) → muted "Call ended · 5m",
    no Join; non-call entry → unchanged title/replies row. (mirrors
    `call-anchor-card.test.ts` SSR harness)
  - `knip` clean.

## Out of scope

- DM 1:1 call threads (different slice; anchors here are `kind: "muc"` only).
- `TopicsPanel` / channel-sidebar thread-list rows (follow-up; not the deferred AC
  surface).
- Server-restart durability of in-flight call anchors (pre-existing limitation of
  the in-memory `call_threads` map; unchanged here).
