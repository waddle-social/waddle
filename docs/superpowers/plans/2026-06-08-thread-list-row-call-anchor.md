# Thread-list-row call anchor (issue #919, AC #4) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a MUC call-thread anchor's row in the global Threads view render the live/ended `CallAnchorCard` (sharing the one live-state composable + Join path), by surfacing the anchor's call-thread metadata through the `urn:waddle:threads:0` query.

**Architecture:** Capture the anchor's typed `kind`+`media` onto the inbox row at the existing groupchat thread-projection point; correlate the XEP-0422 ended fastening back to the thread via a `thread_id` added to the in-memory `ActiveCallThread`, then persist `ended`+`duration` with one cross-user `UPDATE`. Surface all four fields through `ThreadEntry` → `WaddleThreadEntry` → `WasmThreadEntry`, and feed them into the existing `useCallAnchorCardState` composable via a pure adapter consumed by `ThreadsListRow.vue`.

**Tech Stack:** Rust (`waddle-xmpp`, `waddle-server`, `waddle-xmpp-client-wasm`), SQLite/Postgres via the project DB layer, TypeScript/Vue 3 (`chat/`), Bun test, `bun run lint` (knip), `cargo test`, `cargo clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-06-08-thread-list-row-call-anchor-design.md`

**Branch:** `issue-919-live-call-anchor` (PR #926). Commit after every task. Run `cargo fmt` before every Rust commit (server/CLAUDE.md). Never `unwrap`; never add clippy `allow`.

---

## Conventions for the implementer

- Reuse the existing typed call-thread values — do NOT introduce `String` blobs for protocol data:
  - `CallThreadKind` (`Dm`/`Muc`), `CallThreadMedia { audio: bool, video: bool }`,
    `CallThreadDuration` — all in `server/crates/waddle-xmpp/src/xep/xep_waddle_call_thread.rs`
    (re-exported via `xep::exports`). The anchor parser `parse_call_thread_anchor_child(&Message)` and
    the ended parser `parse_call_thread_ended_child(&Message)` already exist there.
- Storage serializes typed values to `String`/`i64` only at the SQL boundary (in `inbox/codec.rs`).
- Each task is TDD: failing test → run (fail) → implement → run (pass) → `cargo fmt` (Rust) → commit.

---

## Task 1: `InboxEntry` carries typed call-thread metadata

**Files:**
- Modify: `server/crates/waddle-xmpp/src/inbox/mod.rs:88-164` (struct + `new` + builders)
- Test: `server/crates/waddle-xmpp/src/inbox/tests.rs`

- [ ] **Step 1: Write the failing test** (append to `inbox/tests.rs`)

```rust
#[test]
fn inbox_entry_carries_call_thread_anchor_and_ended_metadata() {
    use crate::xep::exports::{CallThreadDuration, CallThreadKind, CallThreadMedia};
    let entry = InboxEntry::new(
        "general@conference.example.com".parse().unwrap(),
        ConversationKind::MucRoom,
        "stanza-1",
        1_700_000_000,
    )
    .with_thread("call-thread-uuid")
    .with_call_thread(CallThreadKind::Muc, CallThreadMedia { audio: true, video: true })
    .with_call_ended(
        chrono::DateTime::parse_from_rfc3339("2026-06-07T14:35:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        CallThreadDuration::parse("PT5M").unwrap(),
    );

    assert_eq!(entry.call_thread_kind, Some(CallThreadKind::Muc));
    assert_eq!(entry.call_thread_media, Some(CallThreadMedia { audio: true, video: true }));
    assert_eq!(entry.call_duration, Some(CallThreadDuration::parse("PT5M").unwrap()));
    assert!(entry.call_ended_at.is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd server && cargo test -p waddle-xmpp inbox_entry_carries_call_thread -- --nocapture`
Expected: FAIL — `no method named with_call_thread` / unknown fields.

- [ ] **Step 3: Implement** — add fields to `InboxEntry` (after `author`):

```rust
    /// Call-thread anchor kind, when this thread's root is a call anchor.
    pub call_thread_kind: Option<CallThreadKind>,
    /// Call-thread anchor media, when this thread's root is a call anchor.
    pub call_thread_media: Option<CallThreadMedia>,
    /// When the call ended (XEP-0422 ended fastening), if it has ended.
    pub call_ended_at: Option<DateTime<Utc>>,
    /// Call duration from the ended fastening.
    pub call_duration: Option<CallThreadDuration>,
```

Add imports at top of `mod.rs`:
```rust
use crate::xep::exports::{CallThreadDuration, CallThreadKind, CallThreadMedia};
use chrono::{DateTime, Utc};
```

Initialize all four to `None` in `InboxEntry::new`. Add builders:
```rust
    pub fn with_call_thread(mut self, kind: CallThreadKind, media: CallThreadMedia) -> Self {
        self.call_thread_kind = Some(kind);
        self.call_thread_media = Some(media);
        self
    }

    pub fn with_call_ended(mut self, ended: DateTime<Utc>, duration: CallThreadDuration) -> Self {
        self.call_ended_at = Some(ended);
        self.call_duration = Some(duration);
        self
    }
```

If `CallThreadKind`/`CallThreadMedia`/`CallThreadDuration` do not derive `Serialize`/`Deserialize` (needed because `InboxEntry` derives them), add `#[derive(Serialize, Deserialize)]` to those types in `xep_waddle_call_thread.rs` (they already derive `Debug, Clone, PartialEq, Eq`). Verify they round-trip via serde; `CallThreadMedia` is a plain struct, `CallThreadKind` an enum, `CallThreadDuration` likely wraps a `String` — derive serde on each.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd server && cargo test -p waddle-xmpp inbox_entry_carries_call_thread`
Expected: PASS.

- [ ] **Step 5: cargo fmt + commit**

```bash
cd server && cargo fmt
git add server/crates/waddle-xmpp/src/inbox/mod.rs server/crates/waddle-xmpp/src/inbox/tests.rs server/crates/waddle-xmpp/src/xep/xep_waddle_call_thread.rs
git commit -m "feat(server): carry typed call-thread metadata on InboxEntry"
```

---

## Task 2: Inbox storage — schema columns, codec, upsert, select, mark-ended

**Files:**
- Modify: `server/crates/waddle-server/src/inbox/schema.rs` (add 4 columns + idempotent ALTER migrations)
- Modify: `server/crates/waddle-server/src/inbox/codec.rs` (`SELECT_COLS`, row decode, value encode)
- Modify: `server/crates/waddle-server/src/inbox.rs` (UPSERT bindings + `mark_call_thread_ended`)
- Modify: `server/crates/waddle-server/src/inbox/mod.rs` or the `InboxStorage` trait def (add `mark_call_thread_ended`)
- Test: `server/crates/waddle-server/src/inbox/tests.rs`

### 2a — schema columns

- [ ] **Step 1:** In `schema.rs`, add to BOTH the migration `CREATE TABLE inbox_entries_new` and the `CREATE TABLE IF NOT EXISTS inbox_entries` (before `PRIMARY KEY`):

```sql
                call_thread_kind TEXT,
                call_thread_media TEXT,
                call_ended_at {i64_type},
                call_duration TEXT,
```

(In the migration `inbox_entries_new` block there is no `{i64_type}` interpolation — use `INTEGER` there to match its literal `last_updated INTEGER`.)

- [ ] **Step 2:** Add an idempotent migration helper mirroring `migrate_recovery_channel_broadcast_column`, named `migrate_inbox_call_thread_columns`, that for SQLite probes `PRAGMA table_info(inbox_entries)` per column and `ALTER TABLE inbox_entries ADD COLUMN <c> <type>` when missing, and for Postgres runs `ALTER TABLE inbox_entries ADD COLUMN IF NOT EXISTS <c> <type>`. Columns/types: `call_thread_kind TEXT`, `call_thread_media TEXT`, `call_ended_at BIGINT` (use `i64_type` on Postgres path), `call_duration TEXT`. Call it from `initialize` after the `CREATE INDEX` for threads. Factor the SQLite column probe out of `recovery_column_present_sqlite` into a reusable `column_present_sqlite(storage, table, column)`.

### 2b — codec + upsert + select

- [ ] **Step 3: Write the failing storage test** (`inbox/tests.rs`)

```rust
#[tokio::test]
async fn upsert_persists_call_thread_metadata_and_mark_ended_updates_all_users() {
    use waddle_xmpp::xep::exports::{CallThreadDuration, CallThreadKind, CallThreadMedia};
    let storage = new_test_storage().await; // follow the existing test-setup helper in this file
    let room: BareJid = "general@conference.example.com".parse().unwrap();
    let alice: BareJid = "alice@example.com".parse().unwrap();
    let bob: BareJid = "bob@example.com".parse().unwrap();

    let anchor = |owner: &BareJid| {
        InboxEntry::new(room.clone(), ConversationKind::MucRoom, "anchor-stanza", 1_700_000_000)
            .with_thread("call-thread-uuid")
            .with_call_thread(CallThreadKind::Muc, CallThreadMedia { audio: true, video: false });
        // owner param documents intent; upsert is keyed by user below
    };
    let _ = anchor;

    storage.upsert(&alice, anchor_entry(&room), true).await.unwrap();
    storage.upsert(&bob, anchor_entry(&room), true).await.unwrap();

    let alice_threads = storage.list_all_threads(&alice).await.unwrap();
    let entry = alice_threads.iter().find(|e| e.thread_id.as_deref() == Some("call-thread-uuid")).unwrap();
    assert_eq!(entry.call_thread_kind, Some(CallThreadKind::Muc));
    assert_eq!(entry.call_thread_media, Some(CallThreadMedia { audio: true, video: false }));
    assert!(entry.call_ended_at.is_none());

    let ended = chrono::Utc::now();
    storage
        .mark_call_thread_ended(&room, "call-thread-uuid", ended, &CallThreadDuration::parse("PT5M").unwrap())
        .await
        .unwrap();

    for who in [&alice, &bob] {
        let threads = storage.list_all_threads(who).await.unwrap();
        let e = threads.iter().find(|e| e.thread_id.as_deref() == Some("call-thread-uuid")).unwrap();
        assert!(e.call_ended_at.is_some(), "ended marked for {who}");
        assert_eq!(e.call_duration, Some(CallThreadDuration::parse("PT5M").unwrap()));
        // anchor kind/media survived the ended UPDATE
        assert_eq!(e.call_thread_kind, Some(CallThreadKind::Muc));
    }
}

fn anchor_entry(room: &BareJid) -> InboxEntry {
    use waddle_xmpp::xep::exports::{CallThreadKind, CallThreadMedia};
    InboxEntry::new(room.clone(), ConversationKind::MucRoom, "anchor-stanza", 1_700_000_000)
        .with_thread("call-thread-uuid")
        .with_call_thread(CallThreadKind::Muc, CallThreadMedia { audio: true, video: false })
}
```

(Adapt `new_test_storage()` to whatever helper this test file already uses to construct a `DatabaseInboxStorage`.)

- [ ] **Step 4: Run — expect FAIL** (`mark_call_thread_ended` missing; fields not decoded).

Run: `cd server && cargo test -p waddle-server upsert_persists_call_thread_metadata -- --nocapture`

- [ ] **Step 5: Implement codec** — in `codec.rs`:
  - Extend `SELECT_COLS` to append `, call_thread_kind, call_thread_media, call_ended_at, call_duration`.
  - In the row→`InboxEntry` decode, read the four columns and map:
    - `call_thread_kind`: `Option<String>` → `CallThreadKind` (`"muc"`→`Muc`, `"dm"`→`Dm`); reuse the existing kind string<->enum helper if one exists, else add `call_thread_kind_from_str`/`to_str`.
    - `call_thread_media`: `Option<String>` space-joined tokens (`"audio"`, `"video"`) → `CallThreadMedia` (mirror `media_from_str`/`media_to_string` used by the anchor codec).
    - `call_ended_at`: `Option<i64>` epoch secs → `DateTime<Utc>` via `Utc.timestamp_opt`.
    - `call_duration`: `Option<String>` → `CallThreadDuration::parse(...).ok()`.
  - Add an encode helper `call_thread_columns(entry) -> (Option<String>, Option<String>, Option<i64>, Option<String>)` for the upsert bindings.

- [ ] **Step 6: Implement upsert** — in `inbox.rs` UPSERT (lines ~254-316):
  - Add the four columns to the INSERT column list and `VALUES` placeholders, binding via `call_thread_columns(&entry)`.
  - In `ON CONFLICT ... DO UPDATE SET`, use `COALESCE(excluded.call_thread_kind, inbox_entries.call_thread_kind)` and likewise for `call_thread_media` (so a later reply that lacks anchor metadata does NOT wipe it). Do the same `COALESCE` for `call_ended_at` and `call_duration`.

- [ ] **Step 7: Implement `mark_call_thread_ended`** — add to the `InboxStorage` trait and the `DatabaseInboxStorage` impl:

```rust
async fn mark_call_thread_ended(
    &self,
    room: &BareJid,
    thread_id: &str,
    ended: DateTime<Utc>,
    duration: &CallThreadDuration,
) -> Result<(), InboxStorageError>;
```

Impl runs (bind types per the project DB helpers):
```sql
UPDATE inbox_entries
   SET call_ended_at = ?, call_duration = ?
 WHERE partner_jid = ? AND thread_id = ?
```
with `ended.timestamp()`, `duration.as_str()` (or its serialized form), `room.to_string()`, `thread_id`.
Add a no-op default or `unimplemented!`-free stub for any other `InboxStorage` impls (e.g. an in-memory test store) — search for `impl InboxStorage for`.

- [ ] **Step 8: Run — expect PASS.** Then `cargo fmt`.

- [ ] **Step 9: Commit**

```bash
cd server && cargo fmt
git add server/crates/waddle-server/src/inbox.rs server/crates/waddle-server/src/inbox/
git commit -m "feat(server): persist call-thread metadata + mark-ended in inbox storage"
```

---

## Task 3: Capture anchor kind+media at groupchat thread projection

**Files:**
- Modify: `server/crates/waddle-xmpp/src/protocol/room/inbox.rs:148-171` (`thread_projection`, `GroupchatThreadProjection`)
- Modify: the `OutboundEvent::ProjectGroupchatInbox` definition (add call-thread fields to the `thread` projection — it is already a struct field, so extend `GroupchatThreadProjection`)
- Modify: `server/crates/waddle-server/src/server/routes/interpret.rs` (where `ProjectGroupchatInbox` builds the `InboxEntry`) — call `.with_call_thread(kind, media)` when present
- Test: `server/crates/waddle-xmpp/src/protocol/room/` test module (find the existing inbox-handler tests; else add `#[cfg(test)]` here)

- [ ] **Step 1: Write the failing test** — feed a call-thread anchor message through `thread_projection` and assert the projection carries `call_thread_kind = Muc` and the media; feed a plain thread reply and assert `None`.

```rust
#[test]
fn thread_projection_captures_call_thread_anchor_metadata() {
    use crate::xep::exports::{CallThreadAnchor, CallThreadKind, CallThreadMedia, build_call_thread_anchor};
    // Build a groupchat message that is a thread root (id == thread id) carrying the anchor marker.
    let mut msg = /* construct Message with <thread> id="t1", message.id = "t1" */;
    msg.payloads.push(build_call_thread_anchor(&CallThreadAnchor {
        kind: CallThreadKind::Muc,
        sid: /* SessionId */,
        media: CallThreadMedia { audio: true, video: true },
        initiator: "alice@example.com".parse().unwrap(),
        started: chrono::Utc::now(),
    }));
    let projection = thread_projection(&msg).expect("projection");
    assert_eq!(projection.call_thread_kind, Some(CallThreadKind::Muc));
    assert_eq!(projection.call_thread_media, Some(CallThreadMedia { audio: true, video: true }));
}
```

(Model the `Message` construction on the existing tests in this module / `xep_waddle_call_thread.rs`.)

- [ ] **Step 2: Run — expect FAIL** (unknown fields).

Run: `cd server && cargo test -p waddle-xmpp thread_projection_captures_call_thread`

- [ ] **Step 3: Implement**
  - Add to `GroupchatThreadProjection`: `pub call_thread_kind: Option<CallThreadKind>`, `pub call_thread_media: Option<CallThreadMedia>`.
  - In `thread_projection`, after computing title/author, parse the anchor:
    ```rust
    let call_anchor = parse_call_thread_anchor_child(message);
    ```
    (import `parse_call_thread_anchor_child` from the call-thread xep module) and set
    `call_thread_kind: call_anchor.as_ref().map(|a| a.kind)`, `call_thread_media: call_anchor.as_ref().map(|a| a.media.clone())`.
  - Initialize the new fields wherever `GroupchatThreadProjection { .. }` is constructed (search for it).
  - In `interpret.rs`, where the `InboxEntry` is built from `thread`, add:
    ```rust
    if let (Some(kind), Some(media)) = (thread.call_thread_kind, thread.call_thread_media.clone()) {
        entry = entry.with_call_thread(kind, media);
    }
    ```

- [ ] **Step 4: Run — expect PASS.** `cargo fmt`.

- [ ] **Step 5: Commit**

```bash
cd server && cargo fmt
git add server/crates/waddle-xmpp/src/protocol/room/inbox.rs server/crates/waddle-server/src/server/routes/interpret.rs
git commit -m "feat(server): project call-thread anchor metadata into the inbox"
```

---

## Task 4: Correlate the ended fastening to the thread + persist ended/duration

**Files:**
- Modify: `server/crates/waddle-server/src/server/routes/websocket/state.rs` (`ActiveCallThread` += `thread_id`)
- Modify: `server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc_update.rs` (capture thread_id into `ActiveCallThread`; the anchor message builds the `<thread>` so the id is in scope at `build_call_thread_anchor_message` / its caller)
- Modify: `server/crates/waddle-server/src/server/routes/muc_muji_clear.rs` (`maybe_broadcast_call_thread_ended`: after removing the `ActiveCallThread`, call `inbox.mark_call_thread_ended(room, &active.thread_id, ended, &duration)`)
- Test: `server/crates/waddle-server/tests/calls_e2e_ws.rs` (extend the existing `livekit_last_participant_left_fastens_ended_summary_to_call_thread_anchor` or add a sibling that also asserts the inbox/threads-query reflects ended)

- [ ] **Step 1: Write/extend the failing integration test** — after the ended fastening is broadcast, issue a `urn:waddle:threads:0` query (mirror `waddle_threads_query_ws.rs` helpers) and assert the call-thread row comes back with `call_thread_ended` populated (the WASM field added in Task 6) — OR, to keep this task server-internal, assert via a direct `inbox.list_all_threads` lookup that `call_ended_at`/`call_duration` are set. Prefer the storage-level assertion here; the WASM/threads-query end-to-end is covered in Task 5/6.

- [ ] **Step 2: Run — expect FAIL** (thread_id missing on `ActiveCallThread`; ended not persisted).

- [ ] **Step 3: Implement**
  - `ActiveCallThread { anchor_origin_id, started, thread_id }`.
  - At anchor creation in `muc_update.rs`, the `thread_id` UUID is generated in `build_call_thread_anchor_message`. Return it (or extract from the built message's `<thread>`) so the caller stores it in the `call_threads` insert (lines ~332-342). Update the insert to include `thread_id`.
  - In `maybe_broadcast_call_thread_ended` (`muc_muji_clear.rs`), after `let Some((_, active)) = ...call_threads.remove(room_jid)`, compute `ended`/`duration` (already done), then:
    ```rust
    if let Err(error) = state.deps.inbox.mark_call_thread_ended(room_jid, &active.thread_id, ended, &duration).await {
        // log via the existing Log/tracing path used in this file
        tracing::warn!(%room_jid, %error, "failed to mark call-thread ended in inbox");
    }
    ```
    (Use whatever inbox handle `state.deps` exposes — search `deps.inbox` / how `interpret.rs` reaches inbox storage; reuse that path. Do not `unwrap`.)

- [ ] **Step 4: Run — expect PASS.** `cargo fmt`.

- [ ] **Step 5: Commit**

```bash
cd server && cargo fmt
git add server/crates/waddle-server/src/server/routes/websocket/state.rs server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc_update.rs server/crates/waddle-server/src/server/routes/muc_muji_clear.rs server/crates/waddle-server/tests/calls_e2e_ws.rs
git commit -m "feat(server): persist call-thread ended summary onto the thread inbox rows"
```

---

## Task 5: Threads query carries the call-thread fields

**Files:**
- Modify: `server/crates/waddle-server/src/threads/query.rs` (`ThreadEntry`)
- Modify: `server/crates/waddle-server/src/threads/storage.rs:247-264` (`build_thread_entry`)
- Test: `server/crates/waddle-server/tests/waddle_threads_query_ws.rs`

- [ ] **Step 1: Write the failing test** — seed an inbox call-thread anchor (+ optionally mark ended), run the threads-query WS flow, assert the returned entry exposes the call-thread anchor kind/media and (when ended) ended/duration. Mirror the existing helpers in this file.

- [ ] **Step 2: Run — expect FAIL.**

Run: `cd server && cargo test -p waddle-server --test waddle_threads_query_ws -- --nocapture`

- [ ] **Step 3: Implement** — add to `ThreadEntry`:
```rust
    pub call_thread_kind: Option<CallThreadKind>,
    pub call_thread_media: Option<CallThreadMedia>,
    pub call_ended_at: Option<DateTime<Utc>>,
    pub call_duration: Option<CallThreadDuration>,
```
and in `build_thread_entry` copy them from the `InboxEntry` row (`row.call_thread_kind.clone()`, etc.).

- [ ] **Step 4: Run — expect PASS.** `cargo fmt`.

- [ ] **Step 5: Commit**

```bash
cd server && cargo fmt
git add server/crates/waddle-server/src/threads/
git commit -m "feat(server): expose call-thread fields on threads-query entries"
```

---

## Task 6: WASM bridge — `WaddleThreadEntry` call-thread fields + d.ts

**Files:**
- Modify: `server/crates/waddle-xmpp-client-wasm/src/types.rs:870-886` (`WaddleThreadEntry`)
- Modify: `server/crates/waddle-xmpp-client-wasm/src/client_account.rs` (ThreadEntry→WaddleThreadEntry conversion)
- Modify (generated): `server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.d.ts` (regenerate or hand-edit to match)
- Test: `server/crates/waddle-xmpp-client-wasm/` test module (add a serde-shape test if the crate has one; else assert in `client_account` conversion test)

- [ ] **Step 1: Write the failing test** — construct a `ThreadEntry` with `call_thread_kind=Muc`, media audio+video, ended+duration; convert to `WaddleThreadEntry`; serialize to JSON; assert `callThread.kind == "muc"`, `callThread.media == ["audio","video"]`, `callThreadEnded.ended` + `callThreadEnded.duration == "PT5M"`. For a non-call entry assert both are absent (`skip_serializing_if`).

- [ ] **Step 2: Run — expect FAIL.**

- [ ] **Step 3: Implement** — add to `WaddleThreadEntry`:
```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread: Option<WaddleThreadCallAnchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_thread_ended: Option<WaddleThreadCallEnded>,
```
with new serde structs (camelCase via the crate's existing rename convention — match `WaddleCallThreadAnchor` style):
```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleThreadCallAnchor { pub kind: String, pub media: Vec<String> }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleThreadCallEnded { pub ended: String, pub duration: String }
```
In `client_account.rs` conversion, map `kind` via `match { Muc => "muc", Dm => "dm" }`, media via the existing media→`Vec<String>` helper (`["audio"]`/`["audio","video"]`), `ended` to RFC 3339, `duration` to its string form.

- [ ] **Step 4: Regenerate the d.ts** — run the project's wasm build/typegen (check `server/` scripts / `justfile` / `Makefile`; e.g. `wasm-pack`/`wasm-bindgen` step). If typegen is manual, add to the `WaddleThreadEntry` interface in the `.d.ts`:
```ts
  callThread?: { kind: string; media: string[] };
  callThreadEnded?: { ended: string; duration: string };
```

- [ ] **Step 5: Run — expect PASS.** `cargo fmt`.

- [ ] **Step 6: Commit**

```bash
cd server && cargo fmt
git add server/crates/waddle-xmpp-client-wasm/ server/wasm-pkg/waddle-xmpp-client-wasm/
git commit -m "feat(server): surface call-thread fields on the WASM thread entry"
```

---

## Task 7: TS contract + `wasmThreadEntryToAnchorMessage` adapter

**Files:**
- Modify: `chat/src/lib/xmpp/wasm-types.ts:372-384` (`WasmThreadEntry`)
- Modify: `chat/src/lib/call-thread-anchor.ts` (add adapter)
- Test: `chat/tests/call-thread-anchor.test.ts`

- [ ] **Step 1: Write the failing test** (append to `call-thread-anchor.test.ts`)

```ts
import { wasmThreadEntryToAnchorMessage } from "../src/lib/call-thread-anchor";
import type { WasmThreadEntry } from "../src/lib/xmpp/wasm-types";

const baseEntry: WasmThreadEntry = {
  channel: "general@conference.example.com",
  thread_id: "call-thread-uuid",
  last_stanza_id: "s1",
  last_activity: "2026-06-07T14:30:00Z",
  unread: 0,
  reply_count: 7,
  has_unread: false,
};

test("adapts a live MUC call-thread entry into an anchor-shaped message", () => {
  const msg = wasmThreadEntryToAnchorMessage({
    ...baseEntry,
    callThread: { kind: "muc", media: ["audio", "video"] },
  });
  expect(msg).toEqual({
    body: "",
    author: "",
    threadId: "call-thread-uuid",
    callThread: { kind: "muc", media: ["audio", "video"] },
  });
});

test("adapts an ended MUC call-thread entry with duration", () => {
  const msg = wasmThreadEntryToAnchorMessage({
    ...baseEntry,
    callThread: { kind: "muc", media: ["audio"] },
    callThreadEnded: { ended: "2026-06-07T14:35:00Z", duration: "PT5M" },
  });
  expect(msg?.callThread).toEqual({
    kind: "muc",
    media: ["audio"],
    ended: "2026-06-07T14:35:00Z",
    duration: "PT5M",
  });
});

test("returns null for non-call and non-muc entries", () => {
  expect(wasmThreadEntryToAnchorMessage(baseEntry)).toBeNull();
  expect(
    wasmThreadEntryToAnchorMessage({ ...baseEntry, callThread: { kind: "dm", media: ["audio"] } }),
  ).toBeNull();
});
```

- [ ] **Step 2: Run — expect FAIL.**

Run: `cd chat && bun test tests/call-thread-anchor.test.ts`

- [ ] **Step 3: Implement** — in `wasm-types.ts`, add to `WasmThreadEntry`:
```ts
  callThread?: { kind: "muc" | "dm"; media: ("audio" | "video")[] };
  callThreadEnded?: { ended: string; duration: string };
```
In `call-thread-anchor.ts`, add:
```ts
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";

export function wasmThreadEntryToAnchorMessage(
  entry: WasmThreadEntry,
): Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId"> | null {
  if (!entry.callThread || entry.callThread.kind !== "muc") return null;
  return {
    body: "",
    author: "",
    threadId: entry.thread_id,
    callThread: {
      kind: "muc",
      media: entry.callThread.media,
      ...(entry.callThreadEnded
        ? { ended: entry.callThreadEnded.ended, duration: entry.callThreadEnded.duration }
        : {}),
    },
  };
}
```
(Confirm `CallThreadAnchor` in `chat-ui.ts` requires `sid`/`initiator`/`started`. If those are non-optional, relax them to optional — they are unused by `readCallAnchorCardState`, which only reads `ended`/`duration`/`media` — or build a minimal object cast through the same `Pick`. Prefer making `sid`/`initiator`/`started` optional on `CallThreadAnchor` since timeline messages always set them but thread-entry-derived anchors do not.)

- [ ] **Step 4: Run — expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add chat/src/lib/xmpp/wasm-types.ts chat/src/lib/call-thread-anchor.ts chat/tests/call-thread-anchor.test.ts
git commit -m "feat(chat): adapt WASM thread entries into call-anchor card state"
```

---

## Task 8: Visible ended duration in the card title

**Files:**
- Modify: `chat/src/lib/call-thread-anchor.ts` (`buildCallAnchorCardState` ended title)
- Test: `chat/tests/call-thread-anchor.test.ts` (extend the existing stale-live test)

- [ ] **Step 1: Write the failing assertion** — in the stale-live `toMatchObject({ status: "ended", ... })` test, after clearing participants set `callThread.ended`+`duration` on the timeline message and assert `title: "Call ended · 5m"`. Add a fresh test:

```ts
test("ended card title includes the formatted duration", () => {
  const timelineMessage = mapLiveRoomMessageToTimeline(session, callAnchor({
    roomJid: "general@conference.example.com",
    callThread: { kind: "muc", sid: "s", media: ["audio"], initiator: "alice@example.com", started: "2026-06-07T14:30:00Z", ended: "2026-06-07T14:35:00Z", duration: "PT5M" },
  }));
  expect(readCallAnchorCardState(timelineMessage, "general@conference.example.com")).toMatchObject({
    status: "ended",
    title: "Call ended · 5m",
  });
});
```

- [ ] **Step 2: Run — expect FAIL** (title is `"Call ended"`).

- [ ] **Step 3: Implement** — in `buildCallAnchorCardState`, replace the ended title:
```ts
    title: live
      ? `Live ${mediaLabel}`
      : callThread.ended && callThread.duration
        ? `Call ended · ${formatCallThreadDuration(callThread.duration)}`
        : "Call ended",
```
(Export/keep `formatCallThreadDuration` accessible within the module — it already exists.)

- [ ] **Step 4: Run — expect PASS** (also re-run the full `call-thread-anchor.test.ts` and `call-anchor-card.test.ts` to confirm no regression to existing title assertions).

- [ ] **Step 5: Commit**

```bash
git add chat/src/lib/call-thread-anchor.ts chat/tests/call-thread-anchor.test.ts
git commit -m "feat(chat): show call duration in ended anchor card title"
```

---

## Task 9: `ThreadsListRow.vue` renders the call card + shared Join

**Files:**
- Modify: `chat/src/components/chat/ThreadsListRow.vue` (render `CallAnchorCard` when entry is a MUC call thread)
- Modify: `chat/src/components/chat/ThreadsListPanel.vue` (forward a `joinCall` event)
- Modify: `chat/src/components/chat/ThreadsView.vue` (route `joinCall` to the shared channel-join path used by the banner)
- Modify: `chat/src/components/chat/ChatReadyShell.vue` (wire `ThreadsView`'s join to the same handler the banner uses; reuse existing `joinChannelCall` wiring)
- Test: `chat/tests/threads-list-row.test.ts` (new — SSR harness copied from `call-anchor-card.test.ts`)

- [ ] **Step 1: Write the failing SSR test** (`tests/threads-list-row.test.ts`) — reuse the `renderVueComponent`/`loadVueComponent` helper pattern from `call-anchor-card.test.ts` (extract it to `tests/helpers/render-vue-sfc.ts` and import from both, to stay DRY). Cases:
  - live MUC entry (`callThread` set, room has an active call via the seeded `$mucCallParticipants` store) → html contains `call-anchor-card__pulse` and `Join` and `7 messages in call chat`.
  - ended MUC entry (`callThreadEnded` set, no active call) → contains `call-anchor-card--ended`, `Call ended`, and NOT `>Join<`.
  - non-call entry → contains the plain `{{ title }}` row, NOT `call-anchor-card`.

  Since the row uses the reactive `useCallAnchorCardState`, drive the nanostores (`$mucCallParticipants.set({...})`, `$mucCallMedia.setKey(...)`) before render, and reset in `afterEach` (mirror `call-thread-anchor.test.ts`).

- [ ] **Step 2: Run — expect FAIL.**

Run: `cd chat && bun test tests/threads-list-row.test.ts`

- [ ] **Step 3: Implement `ThreadsListRow.vue`**
```vue
<script setup lang="ts">
import { computed } from "vue";
import CallAnchorCard from "@/components/calls/CallAnchorCard.vue";
import { useCallAnchorCardState, wasmThreadEntryToAnchorMessage } from "@/lib/call-thread-anchor";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
// ...existing imports/props/emits...
const emit = defineEmits<{
  open: [entry: WasmThreadEntry];
  markRead: [entry: WasmThreadEntry];
  joinCall: [entry: WasmThreadEntry];
}>();

const anchorMessage = computed(() => wasmThreadEntryToAnchorMessage(props.entry));
const isCallThread = computed(() => anchorMessage.value !== null);
const callState = useCallAnchorCardState(
  () => anchorMessage.value ?? { body: "", author: "", threadId: null, callThread: undefined },
  () => props.entry.channel,
  () => props.entry.reply_count,
);
</script>

<template>
  <div class="chat-thread-row ...">
    <CallAnchorCard
      v-if="isCallThread && callState"
      :state="callState"
      @join="emit('joinCall', entry)"
      @open-thread="emit('open', entry)"
    />
    <template v-else>
      <!-- existing button/title/replies/unread markup unchanged -->
    </template>
    <!-- existing mark-read button unchanged -->
  </div>
</template>
```
(Keep the existing prop `entry: WasmThreadEntry` and `markingRead`. Guard `callState` for null.)

- [ ] **Step 4: Implement the join routing** — `ThreadsListPanel.vue` re-emits `joinCall`; `ThreadsView.vue` resolves `entry.channel` → channelId (it already has `resolveChannelIdForRoomJid`) and calls the same prop the banner's Join uses. In `ChatReadyShell.vue`, pass `ThreadsView` a `:on-join-call` that invokes the existing `joinChannelCall` handler (the one `ConversationCallBanner` uses) — do NOT introduce a second join path. Search `joinChannelCall` / the banner's `@join` wiring and reuse it.

- [ ] **Step 5: Run — expect PASS.** Run the whole call + threads suite:

Run: `cd chat && bun test tests/threads-list-row.test.ts tests/call-anchor-card.test.ts tests/call-thread-anchor.test.ts tests/threads-view.test.ts`
Expected: all PASS.

- [ ] **Step 6: knip + commit**

```bash
cd chat && bunx knip
git add chat/src/components/chat/ThreadsListRow.vue chat/src/components/chat/ThreadsListPanel.vue chat/src/components/chat/ThreadsView.vue chat/src/components/chat/ChatReadyShell.vue chat/tests/threads-list-row.test.ts chat/tests/helpers/render-vue-sfc.ts
git commit -m "feat(chat): render live/ended call card on global thread-list rows"
```

---

## Task 10: Full verification + PR finalize

- [ ] **Step 1:** `cd chat && bun test` → all pass.
- [ ] **Step 2:** `cd chat && bun run lint` (knip) → clean.
- [ ] **Step 3:** `cd chat && bunx astro check` → no NEW errors (the pre-existing `cloudflare:workers` diagnostic is allowed).
- [ ] **Step 4:** `cd server && cargo fmt --check`
- [ ] **Step 5:** `cd server && cargo test -p waddle-xmpp -p waddle-server -p waddle-xmpp-client-wasm` (at least the call-thread, inbox, threads-query, calls_e2e_ws suites) → all pass.
- [ ] **Step 6:** `cd server && cargo clippy -p waddle-xmpp -p waddle-server -p waddle-xmpp-client-wasm -- -D warnings` → clean.
- [ ] **Step 7:** Adversarial review (per CLAUDE.md): dispatch reviewer sub-agents (protocol-conformance, Rust/storage correctness, TS/Vue) over the diff; fix real findings; repeat until clean.
- [ ] **Step 8:** Update PR #926 title/description to describe the complete work (frontend + server), mark ready for review (remove draft), keep the spec link; verify rendered body with `gh pr view`.
- [ ] **Step 9:** Push; monitor CI (`waddle-chat-pullRequest`, nix gates, CodeQL) until all green; fix failures.

---

## Self-review notes (spec coverage)

- AC #4 global thread-list row live/ended + shared Join → Tasks 1–9 (9 wires the row + shared join).
- "No new wire data" → Tasks 3/4 reuse existing markers; only the `urn:waddle:threads:0` response shape grows (Tasks 5/6/7).
- Typed payloads → Tasks 1/3/5/6 keep `CallThreadKind`/`CallThreadMedia`/`CallThreadDuration` typed; strings only in codec (Task 2) and WASM boundary (Task 6).
- Ended duration visible → Task 8.
- XEP custom test-suite rule → Rust tests in Tasks 1–6; TS tests in 7–9.
- knip clean / clippy -D warnings → Task 10.
- Type-name consistency: `with_call_thread`, `with_call_ended`, `mark_call_thread_ended`, `call_thread_kind`/`call_thread_media`/`call_ended_at`/`call_duration`, `WaddleThreadCallAnchor`/`WaddleThreadCallEnded`, `callThread`/`callThreadEnded` (TS), `wasmThreadEntryToAnchorMessage` — used consistently across tasks.
