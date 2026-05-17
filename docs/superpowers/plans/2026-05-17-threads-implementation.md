# Threads view implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a global "Threads" view in the Waddle chat client, backed by a new `urn:waddle:threads:0` IQ query, so users can see at-a-glance which threads have unread messages and which have recent activity.

**Architecture:** Server reads the existing `inbox_entries` table (already keyed `(user, partner, thread_id)`) and exposes a paginated thread list via a new IQ shape on the user's bare JID. The chat client adds a top-level sidebar entry + a panel that partitions the result client-side into two sections (Unread on top, Following below). No new DB schema. No new advert beyond the disco feature for the namespace.

**Tech Stack:**
- Rust (server crates: `waddle-server`, `waddle-xmpp-client`, `waddle-xmpp-client-wasm`)
- TypeScript / Astro / Vue 3 / Tailwind 4 (`chat/`)
- Bun (chat tooling)
- `minidom::Element` for all XML construction (CLAUDE.md hard rule — never `format!` for XML)
- `cargo fmt`, `cargo clippy -D warnings`, `bun run lint` (knip) — must all be clean before push

Spec: `docs/superpowers/specs/2026-05-17-threads-design.md` (same branch, first commit).

PR: #671 (draft) at `feat/server-waddle-threads-query`.

---

## Pre-work for the agent

Before starting Task 1, read these files end-to-end to internalize the patterns this plan mirrors:

1. `server/crates/waddle-xmpp/src/xep/xep0430.rs` — the existing Waddle inbox IQ shape. The threads query is a sibling Waddle-namespaced query with similar response semantics.
2. `server/crates/waddle-server/src/inbox.rs` — DB read patterns, `InboxStorage` trait, query helpers.
3. `server/crates/waddle-server/src/inbox/codec.rs` — DB-row → typed-value decode (`decode_row` and `SELECT_COLS`).
4. The most recent merged PR that added a new IQ query — likely #664 (push enforcement) or #669 (vcard4 access). Skim how the IQ handler hooks into the WebSocket router.

CLAUDE.md project rules that apply to every task:
- `cargo fmt` before every commit (server/CLAUDE.md hard rule — what tripped #663/#664/#665 in CI).
- No `unwrap` in new code.
- No clippy allows.
- No `format!` for XML — use `minidom::Element` builders.
- Typed payloads everywhere; no string-blob protocol data.
- Conventional Commits, single scope per commit.

---

## Task 1: Server types + module scaffold

**Files:**
- Create: `server/crates/waddle-server/src/threads/mod.rs`
- Create: `server/crates/waddle-server/src/threads/query.rs`
- Modify: `server/crates/waddle-server/src/lib.rs` (add `pub mod threads;` alongside existing `pub mod inbox;`)

- [ ] **Step 1: Add module declaration**

In `server/crates/waddle-server/src/lib.rs`, find the line `pub mod inbox;` and add directly after it:

```rust
pub mod threads;
```

- [ ] **Step 2: Create the module entry file**

Create `server/crates/waddle-server/src/threads/mod.rs`:

```rust
//! Waddle threads — per-thread cross-channel aggregation view.
//!
//! This module carries the IQ-level protocol for the global threads view:
//! a single query that returns an ordered list of threads the user has
//! participated in or has unread in, across every channel.
//!
//! Wire shape: `urn:waddle:threads:0`. See
//! `docs/superpowers/specs/2026-05-17-threads-design.md` for the contract.
//!
//! Data source: the existing `inbox_entries` table (rows with non-empty
//! `thread_id`). No new schema.

pub mod query;
pub mod storage;
pub mod wire;
```

- [ ] **Step 3: Create the typed query types**

Create `server/crates/waddle-server/src/threads/query.rs`:

```rust
//! Typed request and response values for the threads query.

use jid::BareJid;

/// `urn:waddle:threads:0` namespace.
pub const NS_THREADS: &str = "urn:waddle:threads:0";

/// Maximum entries the server will return on a single page when the
/// client omits `<set><max>`. Mirrors the existing inbox cap.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Hard cap on `<set><max>` requested by clients. Same cap as inbox.
pub const MAX_PAGE_SIZE: u32 = 200;

/// A `<query xmlns='urn:waddle:threads:0'/>` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadsQuery {
    /// RSM `<max>` — clamped to `MAX_PAGE_SIZE` at parse time.
    pub page_size: Option<u32>,
    /// RSM `<after>` cursor — opaque string the server emitted previously.
    pub after_cursor: Option<String>,
}

/// One row in a `<threads>` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadEntry {
    pub channel: BareJid,
    pub thread_id: String,
    pub last_stanza_id: String,
    /// Unix seconds since epoch.
    pub last_activity_secs: i64,
    pub unread: u32,
    pub reply_count: u32,
    pub root_author: Option<BareJid>,
    pub preview: Option<String>,
    pub thread_title: Option<String>,
}

impl ThreadEntry {
    pub fn has_unread(&self) -> bool {
        self.unread > 0
    }
}

/// Full response payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadsPage {
    pub entries: Vec<ThreadEntry>,
    pub total: u64,
    pub unread_threads: u64,
    /// `<first>` cursor from RSM, opaque to clients.
    pub first_cursor: Option<String>,
    /// `<last>` cursor from RSM.
    pub last_cursor: Option<String>,
}

/// Errors returned by threads stanza parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ThreadsError {
    #[error("expected <{0}/> in '{NS_THREADS}'")]
    ExpectedElement(&'static str),
    #[error("invalid integer '{0}'")]
    InvalidInteger(String),
    #[error("payload is not the expected IQ type")]
    WrongIqType,
}
```

- [ ] **Step 4: Verify compiles**

Run from the worktree root:
```bash
cargo check -p waddle-server
```
Expected: PASS (warnings about unused `storage`/`wire` modules are OK; we'll fill them next).

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-server/src/lib.rs server/crates/waddle-server/src/threads/
cargo fmt
git add -u server/crates/waddle-server/src/threads/
git commit -m "feat(server): scaffold urn:waddle:threads:0 module with typed query/response

Adds the empty module tree and typed request/response values for the
new global threads view. Subsequent commits add the wire builders,
storage read, IQ handler, and tests.

This commit was created with the assistance of a LLM."
```

---

## Task 2: Wire builders + parser

**Files:**
- Create: `server/crates/waddle-server/src/threads/wire.rs`
- Test inside the same file as `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the wire builder skeleton**

Create `server/crates/waddle-server/src/threads/wire.rs`:

```rust
//! XML wire shape for `urn:waddle:threads:0`. Built via `minidom::Element`
//! so no XML is ever concatenated as strings (CLAUDE.md hard rule).

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

use super::query::{ThreadEntry, ThreadsError, ThreadsPage, ThreadsQuery, NS_THREADS};

/// XEP-0059 Result Set Management namespace.
const NS_RSM: &str = "http://jabber.org/protocol/rsm";

/// Parse a `<query xmlns='urn:waddle:threads:0'/>` IQ payload into a
/// `ThreadsQuery`. The IQ MUST be a `get` for this to succeed.
pub fn parse_threads_query(iq: &Iq) -> Result<ThreadsQuery, ThreadsError> {
    let payload = match &iq.payload {
        IqType::Get(el) => el,
        _ => return Err(ThreadsError::WrongIqType),
    };
    if !payload.is("query", NS_THREADS) {
        return Err(ThreadsError::ExpectedElement("query"));
    }

    let mut q = ThreadsQuery::default();
    if let Some(rsm) = payload.get_child("set", NS_RSM) {
        if let Some(max_el) = rsm.get_child("max", NS_RSM) {
            let text = max_el.text();
            let parsed: u32 = text
                .trim()
                .parse()
                .map_err(|_| ThreadsError::InvalidInteger(text.clone()))?;
            q.page_size = Some(parsed);
        }
        if let Some(after_el) = rsm.get_child("after", NS_RSM) {
            let text = after_el.text();
            if !text.is_empty() {
                q.after_cursor = Some(text);
            }
        }
    }
    Ok(q)
}

/// Build the `<threads>` response element for `page`.
pub fn build_threads_response(page: &ThreadsPage) -> Element {
    let mut threads = Element::builder("threads", NS_THREADS)
        .attr("total", page.total.to_string())
        .attr("unread-threads", page.unread_threads.to_string())
        .build();

    for entry in &page.entries {
        threads.append_child(build_thread_entry(entry));
    }

    let mut set = Element::builder("set", NS_RSM).build();
    if let Some(ref first) = page.first_cursor {
        let mut first_el = Element::builder("first", NS_RSM).build();
        first_el.append_text_node(first);
        set.append_child(first_el);
    }
    if let Some(ref last) = page.last_cursor {
        let mut last_el = Element::builder("last", NS_RSM).build();
        last_el.append_text_node(last);
        set.append_child(last_el);
    }
    let mut count_el = Element::builder("count", NS_RSM).build();
    count_el.append_text_node(page.total.to_string());
    set.append_child(count_el);
    threads.append_child(set);

    threads
}

fn build_thread_entry(entry: &ThreadEntry) -> Element {
    let last_activity_iso = chrono::DateTime::<chrono::Utc>::from_timestamp(
        entry.last_activity_secs,
        0,
    )
    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    let mut t = Element::builder("thread", NS_THREADS)
        .attr("channel", entry.channel.to_string())
        .attr("thread-id", entry.thread_id.clone())
        .attr("last-stanza-id", entry.last_stanza_id.clone())
        .attr("last-activity", last_activity_iso)
        .attr("unread", entry.unread.to_string())
        .attr("reply-count", entry.reply_count.to_string())
        .attr("has-unread", if entry.has_unread() { "true" } else { "false" })
        .build();

    if let Some(ref author) = entry.root_author {
        let mut author_el = Element::builder("root-author", NS_THREADS).build();
        author_el.append_text_node(author.to_string());
        t.append_child(author_el);
    }
    if let Some(ref preview) = entry.preview {
        let mut preview_el = Element::builder("preview", NS_THREADS).build();
        preview_el.append_text_node(preview);
        t.append_child(preview_el);
    }
    if let Some(ref title) = entry.thread_title {
        let mut title_el = Element::builder("thread-title", NS_THREADS).build();
        title_el.append_text_node(title);
        t.append_child(title_el);
    }
    t
}
```

- [ ] **Step 2: Verify compiles**

```bash
cargo check -p waddle-server
```

Expected: PASS. If `chrono` isn't already a dep of `waddle-server`, the compile will fail. In that case, look at the workspace `Cargo.toml` and existing usage: `grep -rn 'chrono' server/crates/waddle-server/Cargo.toml` — if missing, add `chrono = { workspace = true }` to `[dependencies]`. Re-run `cargo check`.

- [ ] **Step 3: Write the parser round-trip test**

Append this `#[cfg(test)]` module to the bottom of `server/crates/waddle-server/src/threads/wire.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use jid::Jid;
    use xmpp_parsers::iq::Iq;

    fn make_get_iq(payload: Element) -> Iq {
        Iq {
            from: None,
            to: None,
            id: "test".into(),
            payload: IqType::Get(payload),
        }
    }

    #[test]
    fn parse_empty_query() {
        let payload = Element::builder("query", NS_THREADS).build();
        let iq = make_get_iq(payload);
        let q = parse_threads_query(&iq).expect("parses");
        assert_eq!(q.page_size, None);
        assert_eq!(q.after_cursor, None);
    }

    #[test]
    fn parse_query_with_rsm() {
        let xml = "<query xmlns='urn:waddle:threads:0'>\
                     <set xmlns='http://jabber.org/protocol/rsm'>\
                       <max>25</max>\
                       <after>CURSOR-1</after>\
                     </set>\
                   </query>";
        let payload: Element = xml.parse().unwrap();
        let iq = make_get_iq(payload);
        let q = parse_threads_query(&iq).expect("parses");
        assert_eq!(q.page_size, Some(25));
        assert_eq!(q.after_cursor.as_deref(), Some("CURSOR-1"));
    }

    #[test]
    fn parse_rejects_non_get_iq() {
        let payload = Element::builder("query", NS_THREADS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "x".into(),
            payload: IqType::Set(payload),
        };
        assert!(matches!(
            parse_threads_query(&iq),
            Err(ThreadsError::WrongIqType)
        ));
    }

    #[test]
    fn build_empty_page() {
        let page = ThreadsPage::default();
        let elem = build_threads_response(&page);
        assert_eq!(elem.name(), "threads");
        assert_eq!(elem.ns(), NS_THREADS);
        assert_eq!(elem.attr("total"), Some("0"));
        assert_eq!(elem.attr("unread-threads"), Some("0"));
        assert!(elem.children().any(|c| c.name() == "set" && c.ns() == NS_RSM));
    }

    #[test]
    fn build_single_entry_round_trip() {
        let entry = ThreadEntry {
            channel: "room@conference.example".parse().unwrap(),
            thread_id: "t-1".into(),
            last_stanza_id: "S-1".into(),
            last_activity_secs: 1_700_000_000,
            unread: 2,
            reply_count: 5,
            root_author: Some("juliet@example.com".parse().unwrap()),
            preview: Some("Anyone reviewed the doc?".into()),
            thread_title: Some("Q3 planning".into()),
        };
        let page = ThreadsPage {
            entries: vec![entry.clone()],
            total: 1,
            unread_threads: 1,
            first_cursor: Some("F".into()),
            last_cursor: Some("L".into()),
        };
        let elem = build_threads_response(&page);

        let thread_el = elem
            .children()
            .find(|c| c.name() == "thread")
            .expect("has thread");
        assert_eq!(thread_el.attr("channel"), Some("room@conference.example"));
        assert_eq!(thread_el.attr("thread-id"), Some("t-1"));
        assert_eq!(thread_el.attr("unread"), Some("2"));
        assert_eq!(thread_el.attr("has-unread"), Some("true"));
        assert_eq!(
            thread_el
                .get_child("root-author", NS_THREADS)
                .map(|e| e.text()),
            Some("juliet@example.com".into())
        );
    }

    #[test]
    fn has_unread_flag_is_false_when_unread_is_zero() {
        let entry = ThreadEntry {
            channel: "room@conference.example".parse().unwrap(),
            thread_id: "t-1".into(),
            last_stanza_id: "S-1".into(),
            last_activity_secs: 1_700_000_000,
            unread: 0,
            reply_count: 1,
            root_author: None,
            preview: None,
            thread_title: None,
        };
        let page = ThreadsPage {
            entries: vec![entry],
            total: 1,
            unread_threads: 0,
            first_cursor: None,
            last_cursor: None,
        };
        let elem = build_threads_response(&page);
        let thread_el = elem.children().find(|c| c.name() == "thread").unwrap();
        assert_eq!(thread_el.attr("has-unread"), Some("false"));
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p waddle-server --lib threads::wire
```
Expected: 5 tests pass, 0 fail.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -u
git commit -m "feat(server): wire shape builders + parser for urn:waddle:threads:0

Implements parse_threads_query and build_threads_response with XEP-0059
RSM pagination. Includes a 5-test suite covering the empty query, RSM
parameters, IQ-type rejection, empty-response shape, and a single-entry
round-trip with all optional fields populated.

XML is built exclusively via minidom::Element builders — no format! for
wire shape.

This commit was created with the assistance of a LLM."
```

---

## Task 3: Storage — DB read against `inbox_entries`

**Files:**
- Create: `server/crates/waddle-server/src/threads/storage.rs`

- [ ] **Step 1: Read the existing inbox storage**

```bash
sed -n '1,200p' server/crates/waddle-server/src/inbox.rs
```

Identify:
- The `InboxStorage` trait shape
- The DB pool / connection type the inbox uses
- The exact SELECT pattern used for thread-level entries (it's the existing query at line ~109: `SELECT {SELECT_COLS} FROM inbox_entries WHERE user_jid = ? AND partner_jid = ? AND thread_id != '' ORDER BY last_updated DESC`)

You'll model the threads SELECT on this, but without the `partner_jid` filter so it spans channels.

- [ ] **Step 2: Write the storage trait + impl skeleton**

Create `server/crates/waddle-server/src/threads/storage.rs`:

```rust
//! Storage read for the global threads view. Pulls per-thread rows from
//! the existing `inbox_entries` table — no schema changes.

use jid::BareJid;

use super::query::{ThreadEntry, ThreadsPage};
use crate::inbox::{InboxStorageError, SELECT_COLS};

/// Read trait for the threads view. Implementations read from
/// `inbox_entries` (or a fixture for tests).
#[async_trait::async_trait]
pub trait ThreadsStorage: Send + Sync {
    async fn page(
        &self,
        user_jid: &BareJid,
        page_size: u32,
        after_cursor: Option<&str>,
    ) -> Result<ThreadsPage, InboxStorageError>;
}
```

Note: `SELECT_COLS` is currently `pub(super)` on the inbox module. You'll need to re-export it as `pub` from `crate::inbox` so the threads module can read it. Do that as a small edit to `server/crates/waddle-server/src/inbox/codec.rs`: change `pub(super) const SELECT_COLS` to `pub const SELECT_COLS` and add `pub use codec::SELECT_COLS;` to `server/crates/waddle-server/src/inbox.rs` (find the existing `pub use codec::*` or similar; add the line near the top of the module).

Run `cargo check -p waddle-server`.

- [ ] **Step 3: Write the failing storage test**

Append to the bottom of `server/crates/waddle-server/src/threads/storage.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::{ConversationKind, InboxEntry, InboxStorage};

    // Reuse the existing inbox test harness — find the in-memory
    // InboxStorage impl that the inbox tests use (search for
    // `InboxStorage` in `server/crates/waddle-server/src/inbox/tests.rs`)
    // and the same fixture pattern (DB pool + temp file) the inbox
    // integration tests use.

    // [The agent: open `inbox/tests.rs` and use the same test-DB
    // bootstrap function it already uses. Mirror the pattern; do
    // not re-invent.]

    #[tokio::test]
    async fn page_returns_only_thread_rows() {
        // Seed three rows:
        //   - (alice, room1, "")         — no thread, should NOT appear
        //   - (alice, room1, "t1")       — thread
        //   - (alice, room2, "t2")       — thread, different room
        // Expect: page() returns 2 entries.
        // [Implementation depends on the existing test bootstrap pattern;
        //  see comment above.]
    }

    #[tokio::test]
    async fn page_orders_by_last_updated_desc() {
        // Seed two thread rows with different last_updated; assert order.
    }

    #[tokio::test]
    async fn page_paginates_with_cursor() {
        // Seed 3 thread rows; request page_size=2; assert first page
        // returns 2 entries with last_cursor set, next page returns 1.
    }
}
```

The test bodies are intentionally pseudocode — the agent must read `inbox/tests.rs` to find the existing test-DB bootstrap (likely `crate::inbox::tests::make_storage()` or similar) and fill them in. Do NOT invent a new test infrastructure; reuse what's there.

- [ ] **Step 4: Implement `page()` for the SQL backend**

The shape mirrors the existing inbox `list_threads_in_room` function (around line 105 in `inbox.rs`). Implement on whatever struct already implements `InboxStorage` for the SQL backend.

Pseudocode SQL:

```sql
SELECT partner_jid, thread_id, kind, last_stanza_id, last_updated,
       unread, preview, thread_title, reply_count, author
FROM inbox_entries
WHERE user_jid = ?
  AND thread_id != ''
  AND (after_cursor IS NULL OR (last_updated, partner_jid, thread_id) < (?, ?, ?))
ORDER BY last_updated DESC, partner_jid ASC, thread_id ASC
LIMIT ?
```

Cursor format: encode `(last_updated, partner_jid, thread_id)` as a base64-of-JSON or pipe-separated string. Pick whichever the inbox uses if there's a precedent; else `last_updated|partner_jid|thread_id` with `|` escaped via URL-percent-encoding.

Also produce `total` (count of all thread rows for the user) and `unread_threads` (count where `unread > 0`) via a second small query — or a `COUNT(*) FILTER (WHERE ...)` in one SQL round if the DB supports it.

- [ ] **Step 5: Run tests**

```bash
cargo test -p waddle-server --lib threads::storage
```
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add -u
git commit -m "feat(server): threads storage reads inbox_entries WHERE thread_id != ''

Adds the ThreadsStorage trait and SQL-backed implementation that paginates
over per-thread inbox rows. RSM cursor is (last_updated, partner_jid,
thread_id) tuple — seek-based so pagination is stable under concurrent
inserts. Includes tests for thread-only filtering, recency ordering, and
two-page cursor round-trip.

This commit was created with the assistance of a LLM."
```

---

## Task 4: IQ handler + ACL

**Files:**
- Create: `server/crates/waddle-server/src/threads/handler.rs`
- Modify: wherever IQ handlers are registered (`grep -rn 'parse_inbox_query\|is_inbox_iq' server/crates/waddle-server/src/server/routes/websocket/handlers/iq/` to locate the dispatch table, mirror that pattern)

- [ ] **Step 1: Read the inbox IQ handler**

```bash
grep -rn 'parse_inbox_query\|build_inbox_query_result' server/crates/waddle-server/src/server/routes/websocket/handlers/iq/
```

Read the file(s) returned. The threads handler is structurally identical: parse the IQ → ACL-check the sender → call storage → build response → return IQ.

- [ ] **Step 2: Write the handler**

Create `server/crates/waddle-server/src/threads/handler.rs` with a `handle_threads_iq` function that:

1. Parses the IQ via `wire::parse_threads_query`.
2. Confirms the requesting full JID's bare matches the IQ's `to` (or, if no `to`, the connection owner). Reject with `<forbidden/>` via the existing `make_forbidden_iq` helper (find it in the inbox handler).
3. Calls `ThreadsStorage::page(...)` with the parsed query.
4. Wraps the result in an IQ via `wire::build_threads_response`.

Function signature mirrors the inbox handler's — match parameter names and ordering.

- [ ] **Step 3: Wire the handler into the IQ dispatcher**

Find the place where the inbox handler is dispatched (likely an `if is_inbox_iq(iq) { ... }` chain). Add a parallel branch:

```rust
} else if iq_payload_is_query_in_ns(iq, NS_THREADS) {
    let response = crate::threads::handler::handle_threads_iq(state, iq).await;
    /* push response */
}
```

Mirror the exact dispatch pattern in use (the agent: copy-adapt from the inbox branch immediately preceding).

- [ ] **Step 4: Run cargo check + clippy**

```bash
cargo check -p waddle-server
cargo clippy -p waddle-server --all-targets -- -D warnings
```

Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -u
git commit -m "feat(server): IQ handler + dispatch for urn:waddle:threads:0

Routes <iq type='get'><query xmlns='urn:waddle:threads:0'/></iq> through
ThreadsStorage and returns the paginated <threads> response. ACL refuses
with <forbidden/> when the requesting JID's bare doesn't match the
addressed user.

This commit was created with the assistance of a LLM."
```

---

## Task 5: WebSocket integration test

**Files:**
- Create: `server/crates/waddle-server/tests/waddle_threads_query_ws.rs`

This is the dedicated XEP test suite required by the audit hard rule.

- [ ] **Step 1: Read an existing WS integration test for pattern**

```bash
ls server/crates/waddle-server/tests/xep04*.rs
```

Pick one of the recently added: `xep0490_mds_ws.rs` or `xep0292_vcard4_ws.rs` (both landed in the last 24h). Read top-to-bottom. Note the bootstrap helpers (probably in `server/crates/waddle-server/tests/common/` or inline).

- [ ] **Step 2: Write the test cases**

Create `server/crates/waddle-server/tests/waddle_threads_query_ws.rs`:

```rust
//! Integration tests for the urn:waddle:threads:0 IQ.
//!
//! Spec: docs/superpowers/specs/2026-05-17-threads-design.md

mod common;
// or whatever import path the existing tests use — mirror that.

// Cases (each is its own #[tokio::test]):
//
// 1. fresh_account_returns_empty_page
//      - Connect alice@host with no inbox state.
//      - Send <iq type='get'><query xmlns='urn:waddle:threads:0'/></iq>.
//      - Expect <iq type='result'> with <threads total='0' unread-threads='0'>
//        containing only an RSM <set><count>0</count></set>.
//
// 2. returns_threads_alice_has_participated_in
//      - Seed two inbox rows for alice across two rooms, each with a
//        distinct thread_id.
//      - Query.
//      - Expect 2 <thread> entries, sorted by last_updated DESC.
//
// 3. only_thread_rows_appear
//      - Seed one thread row + one non-thread row for the same user.
//      - Query.
//      - Expect 1 <thread> entry (the thread row).
//
// 4. pagination_round_trips
//      - Seed 3 thread rows.
//      - First IQ: <set><max>2</max>.
//      - Expect 2 entries + non-empty <last> cursor.
//      - Second IQ: <set><max>2</max><after>{cursor}</after>.
//      - Expect 1 entry + RSM with count=3.
//
// 5. acl_refuses_cross_user_query
//      - alice connects, asks for bob's threads via to='bob@host'.
//      - Expect <iq type='error'> with <forbidden/>.
//
// 6. unread_threads_count_matches_filter
//      - Seed 3 thread rows: 2 with unread>0, 1 caught up.
//      - Query.
//      - Expect <threads ... unread-threads='2'>.
//      - Confirm each <thread> has the right has-unread attribute.

// Each test body uses the existing test bootstrap (see step 1) — do NOT
// reinvent the WS connection setup.
```

The agent fills in the test bodies once they've read the existing pattern. The case list above is non-negotiable — these are the tests the spec calls for.

- [ ] **Step 3: Run the integration tests**

```bash
cargo test -p waddle-server --test waddle_threads_query_ws
```
Expected: 6 passes.

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add server/crates/waddle-server/tests/waddle_threads_query_ws.rs
git commit -m "test(server): waddle_threads_query_ws — end-to-end IQ matrix

Six cases: empty result, multi-channel listing, thread-row filtering,
RSM pagination cursor round-trip, ACL forbidden for cross-user query,
unread-threads count + has-unread attribute.

This commit was created with the assistance of a LLM."
```

---

## Task 6: Disco advert

**Files:**
- Modify: `server/crates/waddle-xmpp-core/src/disco/info/features.rs`
- Modify: wherever `server_features()` (or equivalent) assembles the disco list

- [ ] **Step 1: Add Feature::threads()**

In `server/crates/waddle-xmpp-core/src/disco/info/features.rs`, find an existing simple feature builder (e.g., `pub fn replies()`) and add adjacent:

```rust
pub fn threads_query() -> Self {
    Self::new("urn:waddle:threads:0")
}
```

(Named `threads_query` rather than `threads` to avoid colliding with the long-since-removed `urn:xmpp:threads:0` namespace.)

- [ ] **Step 2: Wire into the user-server disco list**

Find where the server crate assembles its user-server disco features (likely `server/crates/waddle-server/src/server/...` — grep `Feature::inbox` or `Feature::replies` to locate the list). Add `Feature::threads_query()` alongside.

- [ ] **Step 3: Add a disco test**

Find the existing disco WS test (`grep -rn 'disco.*info' server/crates/waddle-server/tests/`). Add a new case asserting `urn:waddle:threads:0` appears in the user-server's disco#info `<feature>` list. Mirror the pattern.

- [ ] **Step 4: Run**

```bash
cargo test -p waddle-server --test <whatever_test_file_you_extended>
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -u
git commit -m "feat(server): advertise urn:waddle:threads:0 in disco#info

This commit was created with the assistance of a LLM."
```

---

## Task 7: Rust IQ builders + parsers in `waddle-xmpp-client`

**Files:**
- Create: `server/crates/waddle-xmpp-client/src/xep/threads.rs`
- Modify: `server/crates/waddle-xmpp-client/src/xep/mod.rs` (re-export)
- Modify: `server/crates/waddle-xmpp-client/src/lib.rs` (re-export at crate root if siblings are there)

- [ ] **Step 1: Read the sibling for pattern**

```bash
sed -n '1,200p' server/crates/waddle-xmpp-client/src/xep/xep0292.rs
```

That file does for vcard4 exactly what this task does for threads: typed value, IQ builder (`build_publish_vcard4_iq`, `build_fetch_vcard4_iq`), IQ parser (`parse_pep_vcard4`). Mirror that shape.

- [ ] **Step 2: Write the threads module**

Create `server/crates/waddle-xmpp-client/src/xep/threads.rs`:

```rust
//! Client-side typed value + IQ builder/parser for urn:waddle:threads:0.

use minidom::Element;

/// Namespace.
pub const NS_THREADS: &str = "urn:waddle:threads:0";
const NS_CLIENT: &str = "jabber:client";
const NS_RSM: &str = "http://jabber.org/protocol/rsm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadEntry {
    pub channel: String,
    pub thread_id: String,
    pub last_stanza_id: String,
    pub last_activity: String, // RFC 3339
    pub unread: u32,
    pub reply_count: u32,
    pub has_unread: bool,
    pub root_author: Option<String>,
    pub preview: Option<String>,
    pub thread_title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadsPage {
    pub total: u64,
    pub unread_threads: u64,
    pub entries: Vec<ThreadEntry>,
    pub next_cursor: Option<String>,
}

/// Build a `<iq type='get'>` requesting the user's threads.
pub fn build_fetch_threads_iq(
    request_id: &str,
    page_size: Option<u32>,
    after_cursor: Option<&str>,
) -> Element {
    let mut query = Element::builder("query", NS_THREADS).build();
    if page_size.is_some() || after_cursor.is_some() {
        let mut set = Element::builder("set", NS_RSM).build();
        if let Some(max) = page_size {
            let mut max_el = Element::builder("max", NS_RSM).build();
            max_el.append_text_node(max.to_string());
            set.append_child(max_el);
        }
        if let Some(after) = after_cursor {
            let mut after_el = Element::builder("after", NS_RSM).build();
            after_el.append_text_node(after);
            set.append_child(after_el);
        }
        query.append_child(set);
    }

    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("id", request_id)
        .append(query)
        .build()
}

/// Parse the `<iq type='result'>` response into a typed `ThreadsPage`.
pub fn parse_threads_response(iq: &Element) -> Option<ThreadsPage> {
    let threads = iq.get_child("threads", NS_THREADS)?;
    let total: u64 = threads
        .attr("total")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let unread_threads: u64 = threads
        .attr("unread-threads")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let entries: Vec<ThreadEntry> = threads
        .children()
        .filter(|c| c.name() == "thread" && c.ns() == NS_THREADS)
        .map(|t| ThreadEntry {
            channel: t.attr("channel").unwrap_or("").to_string(),
            thread_id: t.attr("thread-id").unwrap_or("").to_string(),
            last_stanza_id: t.attr("last-stanza-id").unwrap_or("").to_string(),
            last_activity: t.attr("last-activity").unwrap_or("").to_string(),
            unread: t.attr("unread").and_then(|s| s.parse().ok()).unwrap_or(0),
            reply_count: t
                .attr("reply-count")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            has_unread: t.attr("has-unread") == Some("true"),
            root_author: t
                .get_child("root-author", NS_THREADS)
                .map(|e| e.text())
                .filter(|s| !s.is_empty()),
            preview: t
                .get_child("preview", NS_THREADS)
                .map(|e| e.text())
                .filter(|s| !s.is_empty()),
            thread_title: t
                .get_child("thread-title", NS_THREADS)
                .map(|e| e.text())
                .filter(|s| !s.is_empty()),
        })
        .collect();

    let next_cursor = threads
        .get_child("set", NS_RSM)
        .and_then(|s| s.get_child("last", NS_RSM))
        .map(|e| e.text())
        .filter(|s| !s.is_empty());

    Some(ThreadsPage {
        total,
        unread_threads,
        entries,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_iq_has_correct_namespace_and_type() {
        let iq = build_fetch_threads_iq("r-1", Some(25), Some("CUR"));
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("id"), Some("r-1"));
        let query = iq.get_child("query", NS_THREADS).expect("query");
        let set = query.get_child("set", NS_RSM).expect("rsm set");
        assert_eq!(
            set.get_child("max", NS_RSM).map(|e| e.text()),
            Some("25".into())
        );
        assert_eq!(
            set.get_child("after", NS_RSM).map(|e| e.text()),
            Some("CUR".into())
        );
    }

    #[test]
    fn parse_extracts_entries_and_cursor() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                     <threads xmlns='urn:waddle:threads:0' total='2' unread-threads='1'>\
                       <thread channel='room@x' thread-id='t1' \
                               last-stanza-id='S1' last-activity='2026-01-01T00:00:00Z' \
                               unread='2' reply-count='5' has-unread='true'>\
                         <preview>hi</preview>\
                       </thread>\
                       <thread channel='room@x' thread-id='t2' \
                               last-stanza-id='S2' last-activity='2025-12-31T00:00:00Z' \
                               unread='0' reply-count='3' has-unread='false'/>\
                       <set xmlns='http://jabber.org/protocol/rsm'>\
                         <last>LAST-CUR</last><count>2</count>\
                       </set>\
                     </threads>\
                   </iq>";
        let iq: Element = xml.parse().unwrap();
        let page = parse_threads_response(&iq).expect("parses");
        assert_eq!(page.total, 2);
        assert_eq!(page.unread_threads, 1);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].thread_id, "t1");
        assert!(page.entries[0].has_unread);
        assert!(!page.entries[1].has_unread);
        assert_eq!(page.next_cursor.as_deref(), Some("LAST-CUR"));
    }

    #[test]
    fn parse_returns_none_when_threads_missing() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'/>";
        let iq: Element = xml.parse().unwrap();
        assert!(parse_threads_response(&iq).is_none());
    }
}
```

- [ ] **Step 3: Re-export from `xep/mod.rs`**

In `server/crates/waddle-xmpp-client/src/xep/mod.rs`, find the existing `pub mod xep0292;` line and add directly after:

```rust
pub mod threads;
```

If the crate root (`lib.rs`) re-exports xep0292 builders for convenience, mirror that for threads.

- [ ] **Step 4: Run tests**

```bash
cargo test -p waddle-xmpp-client --lib xep::threads
cargo clippy -p waddle-xmpp-client --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -u
git commit -m "feat(server): waddle-xmpp-client typed value + IQ builders for threads

Adds ThreadEntry, ThreadsPage, build_fetch_threads_iq, and
parse_threads_response in xep/threads.rs — the client-side mirror of
the server's wire shape. Three unit tests cover IQ build, full
response parse, and the missing-element branch.

This commit was created with the assistance of a LLM."
```

---

## Task 8: Wasm bindings — `fetch_threads`

**Files:**
- Modify: `server/crates/waddle-xmpp-client-wasm/src/types.rs`
- Modify: `server/crates/waddle-xmpp-client-wasm/src/client_account.rs` (or wherever account-scoped methods live — grep `fetch_inbox` to find it)
- Modify: `server/crates/waddle-xmpp-client-wasm/src/lib.rs`

- [ ] **Step 1: Read existing wasm method pattern**

```bash
grep -n 'fetch_inbox\|publish_vcard4\|fetch_vcard4' server/crates/waddle-xmpp-client-wasm/src/client_account.rs
```

Open the file and read those methods end-to-end. The threads fetch method is structurally identical: build IQ → send → parse → convert to serde-serializable wasm type → return `Promise`.

- [ ] **Step 2: Add the wasm-facing types**

Append to `server/crates/waddle-xmpp-client-wasm/src/types.rs`:

```rust
/// XEP-style payload for one entry in a urn:waddle:threads:0 response,
/// shaped for JS consumption.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WaddleThreadEntry {
    pub channel: String,
    pub thread_id: String,
    pub last_stanza_id: String,
    /// RFC 3339 timestamp.
    pub last_activity: String,
    pub unread: u32,
    pub reply_count: u32,
    pub has_unread: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_title: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct WaddleThreadsPage {
    pub total: u64,
    pub unread_threads: u64,
    pub entries: Vec<WaddleThreadEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
```

- [ ] **Step 3: Add the wasm method**

In `server/crates/waddle-xmpp-client-wasm/src/client_account.rs` (or the file that hosts `fetch_inbox` — same file), add a `fetch_threads` method mirroring `fetch_inbox`'s structure:

```rust
#[wasm_bindgen]
impl WaddleXmppClient {
    /// Fetch the global threads view (urn:waddle:threads:0).
    #[wasm_bindgen]
    pub fn fetch_threads(
        &self,
        page_size: Option<u32>,
        after_cursor: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let request_id = Uuid::new_v4().to_string();
            let iq = build_fetch_threads_iq(
                &request_id,
                page_size,
                after_cursor.as_deref(),
            );
            let response = inner.send_iq(iq).await.map_err(to_js_err)?;
            let page = parse_threads_response(&response)
                .ok_or_else(|| JsValue::from_str("malformed threads response"))?;
            to_js_value(&WaddleThreadsPage {
                total: page.total,
                unread_threads: page.unread_threads,
                entries: page
                    .entries
                    .into_iter()
                    .map(|e| WaddleThreadEntry {
                        channel: e.channel,
                        thread_id: e.thread_id,
                        last_stanza_id: e.last_stanza_id,
                        last_activity: e.last_activity,
                        unread: e.unread,
                        reply_count: e.reply_count,
                        has_unread: e.has_unread,
                        root_author: e.root_author,
                        preview: e.preview,
                        thread_title: e.thread_title,
                    })
                    .collect(),
                next_cursor: page.next_cursor,
            })
        })
    }
}
```

(Imports `build_fetch_threads_iq`, `parse_threads_response`, `WaddleThreadEntry`, `WaddleThreadsPage`, `Uuid`, `to_js_err`, `to_js_value`, `future_to_promise` as the sibling methods do — copy the import block from the vcard4 method.)

- [ ] **Step 4: Rebuild wasm package**

```bash
cd chat && bun run wasm:build && cd ..
```

This regenerates `server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.{js,d.ts}`.

- [ ] **Step 5: Run checks**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
git add -u
git add server/wasm-pkg/
git commit -m "feat(server): wasm bindings for fetch_threads

Adds WaddleThreadEntry / WaddleThreadsPage and the fetch_threads method
on WaddleXmppClient. Regenerated wasm-pkg bindings included.

This commit was created with the assistance of a LLM."
```

---

## Task 9: TypeScript types + client wrapper

**Files:**
- Modify: `chat/src/lib/xmpp/wasm-types.ts`
- Modify: `chat/src/lib/xmpp/client.ts`

- [ ] **Step 1: Add the TS types**

Append to `chat/src/lib/xmpp/wasm-types.ts`, in alphabetical-by-feature order (find the `WasmVCard4` block and add after it):

```ts
/** One entry in a urn:waddle:threads:0 response. */
export interface WasmThreadEntry {
  channel: string;
  thread_id: string;
  last_stanza_id: string;
  /** RFC 3339 timestamp. */
  last_activity: string;
  unread: number;
  reply_count: number;
  has_unread: boolean;
  root_author?: string;
  preview?: string;
  thread_title?: string;
}

export interface WasmThreadsPage {
  total: number;
  unread_threads: number;
  entries: WasmThreadEntry[];
  next_cursor?: string;
}
```

- [ ] **Step 2: Add the BrowserXmppClient wrapper**

Find `publishVCard4` / `fetchVCard4` in `chat/src/lib/xmpp/client.ts`. Add a sibling `fetchThreads`:

```ts
async fetchThreads(
  pageSize?: number,
  afterCursor?: string,
): Promise<WasmThreadsPage | null> {
  const xmpp = this.xmpp;
  if (!xmpp) return null;
  try {
    const raw = await xmpp.fetch_threads?.(pageSize, afterCursor);
    return (raw as WasmThreadsPage | undefined) ?? null;
  } catch (err) {
    logWarn("fetchThreads failed", err);
    return null;
  }
}
```

(Match the exact error-handling and logger import the sibling vcard4 methods use.)

- [ ] **Step 3: Run checks**

```bash
cd chat
bun run wasm:build
bun test
bun run lint
cd ..
```

Expected: tests pass, knip clean.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "feat(chat): wasm-types and BrowserXmppClient wrapper for threads

This commit was created with the assistance of a LLM."
```

---

## Task 10: ThreadsListPanel + ThreadsListRow Vue components

**Files:**
- Create: `chat/src/components/chat/ThreadsListPanel.vue`
- Create: `chat/src/components/chat/ThreadsListRow.vue`
- Create: `chat/src/pages/threads.astro`

- [ ] **Step 1: Read the inbox page + panel for pattern**

```bash
ls chat/src/pages/ | grep -E 'inbox|dms|index'
```

Open the most similar existing page (likely `chat/src/pages/index.astro` or `chat/src/pages/dm/index.astro`). Read the Astro shell, identify how a Vue island is mounted, what props it gets, what session-context wrappers it uses.

Also read `chat/src/components/chat/UserProfileDrawer.vue` (recently touched in #668) as a Vue-component-with-async-fetch pattern.

- [ ] **Step 2: Create the row component**

Create `chat/src/components/chat/ThreadsListRow.vue`:

```vue
<script setup lang="ts">
import { computed } from "vue";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";

const props = defineProps<{
  entry: WasmThreadEntry;
}>();

const emit = defineEmits<{
  open: [entry: WasmThreadEntry];
}>();

const recencyLabel = computed(() => {
  const ts = Date.parse(props.entry.last_activity);
  if (Number.isNaN(ts)) return "";
  const deltaSec = Math.floor((Date.now() - ts) / 1000);
  if (deltaSec < 60) return "just now";
  if (deltaSec < 3600) return `${Math.floor(deltaSec / 60)}m ago`;
  if (deltaSec < 86_400) return `${Math.floor(deltaSec / 3600)}h ago`;
  return `${Math.floor(deltaSec / 86_400)}d ago`;
});

const channelLabel = computed(() => {
  const local = props.entry.channel.split("@")[0] ?? props.entry.channel;
  return `#${local}`;
});
</script>

<template>
  <button
    type="button"
    class="chat-section-card chat-thread-row glass-panel w-full text-left"
    @click="emit('open', entry)"
  >
    <div class="flex items-center justify-between gap-2">
      <div class="min-w-0 flex-1">
        <div class="type-card-title truncate">
          {{ entry.thread_title ?? entry.preview ?? "Thread" }}
        </div>
        <div class="type-caption text-muted-foreground truncate">
          {{ channelLabel }} · {{ recencyLabel }}
          <span v-if="entry.reply_count > 0"> · {{ entry.reply_count }} replies</span>
        </div>
      </div>
      <span
        v-if="entry.has_unread"
        class="chat-unread-badge"
        :aria-label="`${entry.unread} unread`"
      >
        {{ entry.unread }}
      </span>
    </div>
  </button>
</template>
```

- [ ] **Step 3: Create the panel**

Create `chat/src/components/chat/ThreadsListPanel.vue`:

```vue
<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WasmThreadEntry, WasmThreadsPage } from "@/lib/xmpp/wasm-types";
import ThreadsListRow from "@/components/chat/ThreadsListRow.vue";

const props = defineProps<{
  xmppClient: BrowserXmppClient | null;
}>();

const emit = defineEmits<{
  openThread: [entry: WasmThreadEntry];
}>();

const loading = ref(true);
const page = ref<WasmThreadsPage | null>(null);
const error = ref<string | null>(null);

const unread = computed(
  () => page.value?.entries.filter((e) => e.has_unread) ?? [],
);
const following = computed(
  () => page.value?.entries.filter((e) => !e.has_unread) ?? [],
);

onMounted(async () => {
  if (!props.xmppClient) {
    loading.value = false;
    return;
  }
  try {
    page.value = await props.xmppClient.fetchThreads(50);
  } catch (err) {
    error.value = String(err);
  } finally {
    loading.value = false;
  }
});
</script>

<template>
  <div class="chat-panel-stack p-4">
    <h2 class="type-pane-title">Threads</h2>

    <div v-if="loading" class="chat-panel-stack" aria-busy="true">
      Loading…
    </div>

    <div v-else-if="error" class="type-caption text-destructive">
      Couldn't load threads: {{ error }}
    </div>

    <template v-else-if="page">
      <section v-if="unread.length > 0" class="chat-panel-stack">
        <div class="type-section-label text-muted-foreground/75">
          Unread · {{ unread.length }}
        </div>
        <ThreadsListRow
          v-for="entry in unread"
          :key="`${entry.channel}|${entry.thread_id}`"
          :entry="entry"
          @open="emit('openThread', $event)"
        />
      </section>

      <section v-if="following.length > 0" class="chat-panel-stack">
        <div class="type-section-label text-muted-foreground/75">
          Following · {{ following.length }}
        </div>
        <ThreadsListRow
          v-for="entry in following"
          :key="`${entry.channel}|${entry.thread_id}`"
          :entry="entry"
          @open="emit('openThread', $event)"
        />
      </section>

      <div v-if="unread.length === 0 && following.length === 0" class="type-caption text-muted-foreground">
        No threads yet. Threads you reply to or get mentioned in will show up here.
      </div>
    </template>
  </div>
</template>
```

- [ ] **Step 4: Create the Astro page**

Create `chat/src/pages/threads.astro` by mirroring whatever page mounts the inbox (find via `grep -rn 'InboxPanel\|InboxView' chat/src/pages/`). Apply the same auth-required shell wrapper, mount `<ThreadsListPanel client:only="vue" :xmpp-client="..." @open-thread="..." />`. The `@open-thread` handler delegates to whatever existing controller method opens `ThreadPanel.vue` at `(channel, thread_id)` — check `chat-app-controller.ts` for a `openThread` or similar.

- [ ] **Step 5: Run checks**

```bash
cd chat
bun run wasm:build
bun test
bun run lint
cd ..
```

- [ ] **Step 6: Commit**

```bash
git add chat/
git commit -m "feat(chat): ThreadsListPanel + ThreadsListRow + /threads route

Two-section layout (Unread top, Following below) over the
urn:waddle:threads:0 response. Click opens the existing ThreadPanel
at (channel, thread_id).

This commit was created with the assistance of a LLM."
```

---

## Task 11: Sidebar nav entry

**Files:**
- Modify: the main sidebar component. Locate with `grep -rln 'Inbox\|InboxView\|chat-sidebar' chat/src/components`

- [ ] **Step 1: Find the sidebar**

```bash
grep -rln '"Inbox"\|"DMs"\|chat-sidebar-item' chat/src/components | head -5
```

Open the file. The sidebar is likely declarative — find the array/list of nav entries.

- [ ] **Step 2: Add the Threads entry**

Insert a new entry between Inbox and the channel list:

```ts
{ id: "threads", label: "Threads", icon: ThreadsIcon, href: "/threads",
  badge: computed(() => unreadThreadsCount.value) }
```

(Match the existing entry shape — type signatures, icon component, and badge computation pattern. If badges use a store value, the store needs a `unreadThreadsCount` field — wire that from a periodic `fetchThreads` or from a watcher on inbox writes.)

For V1, if badge wiring is fiddly, leave it as a static `0` or omit, and note as a follow-up.

- [ ] **Step 3: Run checks**

```bash
cd chat && bun test && bun run lint && cd ..
```

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "feat(chat): Threads sidebar nav entry

This commit was created with the assistance of a LLM."
```

---

## Task 12: Vitest for ThreadsListPanel

**Files:**
- Create: `chat/src/components/chat/__tests__/ThreadsListPanel.test.ts`

- [ ] **Step 1: Read an existing component test for pattern**

```bash
ls chat/src/components/chat/__tests__/ 2>/dev/null | head -5
```

If `__tests__` doesn't exist, look for sibling tests via `find chat/src -name '*.test.ts'`. Mirror the testing-library / Vitest pattern in use.

- [ ] **Step 2: Write tests**

Create `chat/src/components/chat/__tests__/ThreadsListPanel.test.ts`:

```ts
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/vue";
import ThreadsListPanel from "@/components/chat/ThreadsListPanel.vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WasmThreadsPage } from "@/lib/xmpp/wasm-types";

function client(page: WasmThreadsPage | null): BrowserXmppClient {
  return {
    fetchThreads: vi.fn(async () => page),
  } as unknown as BrowserXmppClient;
}

describe("ThreadsListPanel", () => {
  it("renders empty state when no threads", async () => {
    render(ThreadsListPanel, {
      props: { xmppClient: client({ total: 0, unread_threads: 0, entries: [] }) },
    });
    expect(await screen.findByText(/No threads yet/i)).toBeTruthy();
  });

  it("splits unread and following sections", async () => {
    render(ThreadsListPanel, {
      props: {
        xmppClient: client({
          total: 2,
          unread_threads: 1,
          entries: [
            {
              channel: "room@x",
              thread_id: "t1",
              last_stanza_id: "S1",
              last_activity: new Date().toISOString(),
              unread: 2,
              reply_count: 3,
              has_unread: true,
              preview: "Hello",
            },
            {
              channel: "room@x",
              thread_id: "t2",
              last_stanza_id: "S2",
              last_activity: new Date().toISOString(),
              unread: 0,
              reply_count: 5,
              has_unread: false,
              preview: "Quiet thread",
            },
          ],
        }),
      },
    });
    expect(await screen.findByText(/Unread · 1/)).toBeTruthy();
    expect(await screen.findByText(/Following · 1/)).toBeTruthy();
  });

  it("emits openThread when a row is clicked", async () => {
    const entry = {
      channel: "room@x",
      thread_id: "t1",
      last_stanza_id: "S1",
      last_activity: new Date().toISOString(),
      unread: 1,
      reply_count: 2,
      has_unread: true,
      preview: "Click me",
    };
    const { emitted, findByText } = render(ThreadsListPanel, {
      props: { xmppClient: client({ total: 1, unread_threads: 1, entries: [entry] }) },
    });
    const row = await findByText(/Click me/);
    await row.click();
    expect(emitted("openThread")).toBeTruthy();
    expect(emitted("openThread")?.[0]?.[0]).toMatchObject({ thread_id: "t1" });
  });
});
```

- [ ] **Step 3: Run tests**

```bash
cd chat && bun test ThreadsListPanel && cd ..
```

- [ ] **Step 4: Commit**

```bash
git add chat/src/components/chat/__tests__/ThreadsListPanel.test.ts
git commit -m "test(chat): ThreadsListPanel happy paths

Empty state, two-section split, click emits openThread.

This commit was created with the assistance of a LLM."
```

---

## Task 13: Audit doc updates

**Files:**
- Modify: `docs/xep-conformance-audit.md`

- [ ] **Step 1: Correct the XEP-0430 row**

In `docs/xep-conformance-audit.md`, find the row beginning `| 0430 | Inbox`. Replace the namespace cell from `urn:xmpp:inbox:0` with:

```
urn:waddle:inbox:0 + erlang-solutions.com:xmpp:inbox:0
```

Update the notes column to:

```
Waddle private surface (urn:waddle:inbox:0) plus ESL/MongooseIM compat (erlang-solutions.com:xmpp:inbox:0). Conformant XEP-0430 (urn:xmpp:inbox:0 / urn:xmpp:inbox:1) migration is a queued follow-up — see plan in this branch's spec doc.
```

Change `Status` from `unaudited` to `gap` (since the disco advert was historically wrong about which namespace was implemented; the threads PR doesn't fix the inbox itself but at least the audit doc now reflects reality).

- [ ] **Step 2: Add the new row for urn:waddle:threads:0**

After the XEP-0513 row (the last row in the table), add:

```
| —    | (Waddle) Threads view                                | urn:waddle:threads:0               |  Y  |  Y   |   Y   | fixed      | Waddle-namespaced (no XEP equivalent — Fluux made the same call for their custom conversation list). Server reads inbox_entries WHERE thread_id != ''; chat client renders two-section view. PR #671. |
```

(XEP number `—` because there is no XEP. The audit doc table header says "XEP" but the row is informative.)

- [ ] **Step 3: Commit**

```bash
git add docs/xep-conformance-audit.md
git commit -m "docs(server): conformance audit — threads view + correct XEP-0430 row

Adds a row for the new urn:waddle:threads:0 query (no XEP equivalent;
Fluux precedent for using a vendor namespace for inbox-like surfaces).
Corrects the XEP-0430 row to match the actual implemented namespaces
(urn:waddle:inbox:0 + ESL compat).

This commit was created with the assistance of a LLM."
```

---

## Task 14: Final verification

- [ ] **Step 1: Full workspace test pass**

```bash
cargo test --workspace
```
Expected: green.

- [ ] **Step 2: Clippy across the workspace**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: zero warnings.

- [ ] **Step 3: cargo fmt check**

```bash
cargo fmt --check
```

- [ ] **Step 4: Chat checks**

```bash
cd chat && bun test && bun run lint && cd ..
```

- [ ] **Step 5: Push the final commit chain**

```bash
git push
```

- [ ] **Step 6: Check CI on PR #671**

```bash
gh pr checks 671
```

Address any failures before reporting back.

---

## Out of plan (V2 candidates the spec acknowledges)

- Filter toggles (Unread-only / All in channels I'm in)
- Thread mute / pin / archive
- Per-channel Threads tab inside a channel
- Live thread-delta event hook — covered in "Task 11 step 2" as a TODO if the inbox-write event path is messy to plumb; if it lands cleanly, fold into Task 10

## Self-review notes

Spec coverage check ran. Every spec section maps to at least one task:
- Goals 1–3 → Tasks 1–13
- Wire protocol → Tasks 1, 2, 7
- Server architecture (module layout, ACL, tests) → Tasks 1, 2, 3, 4, 5
- Client architecture (sidebar, page, components, wasm, live updates) → Tasks 8, 9, 10, 11, 12
- Audit doc → Task 13

Placeholder scan: no TBDs in code blocks. Some tasks (3, 4, 5, 7, 10, 11, 12) deliberately point the agent at existing files to mirror — the patterns are recent (PRs from the last 24h) and discoverable.

Type-name consistency: `ThreadEntry`/`ThreadsPage` (Rust server) ↔ `ThreadEntry`/`ThreadsPage` (Rust client crate) ↔ `WaddleThreadEntry`/`WaddleThreadsPage` (wasm) ↔ `WasmThreadEntry`/`WasmThreadsPage` (TS). Three different naming surfaces but each is internally consistent and the conversion is explicit at each boundary.

Field-name consistency: `has_unread` (snake_case) preserved across Rust ↔ wasm-serde ↔ TS. `last_activity` is RFC 3339 string in client/TS layers (timezone-safe); `last_activity_secs: i64` on the server side. Conversion is in `build_thread_entry`.
