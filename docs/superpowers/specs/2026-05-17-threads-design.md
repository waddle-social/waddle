# Threads view + inbox namespace cleanup

Status: design approved, pre-implementation
Date: 2026-05-17
Author: David Flanagan (with Claude)
Tracking PR: TBD

## Problem

Today a Waddle user has no surface that aggregates threads. The `<thread/>` element (XEP-0201) is preserved on the wire and MAM, the inbox row keys by `(user, partner, thread_id)`, and `ThreadPanel.vue` reads one thread when opened — but there is no view that lists "which threads are active right now" or "which threads have new messages for me." The only thread discovery path today is scrolling a channel and noticing a per-message reply chip.

This spec adds a global Threads view to fix that, and cleans up an unrelated CLAUDE.md hard-rule violation found in the same area along the way.

## Goals

1. A user can open a single global view that lists every thread they have unread in or have participated in, across all channels and DMs.
2. The view visually separates "unread for me" from "active but caught up," so both halves of the original ask ("which are active" / "which have new") are answered at a glance.
3. The new wire shape uses a Waddle-owned namespace so no official XEP namespace is overloaded with Waddle-specific semantics.

## A spec correction made during plan recon

The original draft of this design assumed the existing inbox IQ used `urn:xmpp:inbox:0` (an official XEP-0430 namespace) and that thread extras carried in its `<conversation/>` elements were a CLAUDE.md hard-rule violation.

That was wrong. Waddle's inbox IQ uses `urn:waddle:inbox:0` for the typed query (see `server/crates/waddle-xmpp/src/xep/xep0430.rs:26`) and `erlang-solutions.com:xmpp:inbox:0` for the ESL-compat surface (see `server/crates/waddle-xmpp-client/src/discovery.rs:39`). Neither is an official `urn:xmpp:*` namespace, so carrying Waddle thread fields inside them is not a violation.

The only artifact that still calls the inbox `urn:xmpp:inbox:0` is one row in `docs/xep-conformance-audit.md` — a doc bug. This PR corrects that row but otherwise leaves the inbox surface alone.

## Non-goals

- Thread mute / pin / archive.
- "All threads in channels I'm in" filter — only participated-or-unread in V1.
- Replacing or restructuring `ThreadPanel.vue` (the reader). Threads view *opens* the existing panel.
- Backfilling thread participation state for accounts that have none — empty view with helpful text is fine.
- New thread creation UX (threads are created today by replying with `<thread/>` and that stays).
- Cross-device PEP-push synchronization à la Fluux. The data already lives server-side; clients query fresh.

## Decisions

| Question | Decision | Source |
|---|---|---|
| Surface | Global "Threads" entry in main sidebar (peer of Inbox/DMs) | User pick, mockup A in brainstorm screen 01 |
| Filter | Threads I've participated in OR have unread (Slack model) | User pick, AskUserQuestion 1 |
| "Active" means | Recent activity timestamp (server-derivable, not presence-based) | User pick, AskUserQuestion 2 |
| Layout | Two sections — Unread pinned top, Following below; each ordered by `last_activity DESC` | User pick, mockup B in brainstorm screen 02 |
| Protocol shape | New `urn:waddle:threads:0` query (Option A) | User approval after Fluux review |
| Inbox cleanup | Dropped — there is no namespace violation (see "A spec correction" above). Only the audit doc row needs fixing. | Recon during plan |

## Wire protocol

### New: `urn:waddle:threads:0` query

The chat client issues this to the user's bare JID. Server applies the participated-or-unread filter implicitly.

Request:

```xml
<iq type='get' id='th1'>
  <query xmlns='urn:waddle:threads:0'>
    <set xmlns='http://jabber.org/protocol/rsm'>
      <max>50</max>
      <after>OPAQUE-CURSOR</after>     <!-- optional, omitted on first page -->
    </set>
  </query>
</iq>
```

Response:

```xml
<iq type='result' id='th1'>
  <threads xmlns='urn:waddle:threads:0' total='12' unread-threads='2'>
    <thread channel='room@conference.example'
            thread-id='t-abc'
            last-stanza-id='SID'
            last-activity='2026-05-17T15:42:08Z'
            unread='2'
            reply-count='12'
            has-unread='true'>
      <root-author>juliet@example</root-author>
      <preview>Anyone reviewed the doc?</preview>
      <thread-title>Q3 planning</thread-title>
    </thread>
    <!-- ...more entries... -->
    <set xmlns='http://jabber.org/protocol/rsm'>
      <first>FIRST-CURSOR</first>
      <last>LAST-CURSOR</last>
      <count>12</count>
    </set>
  </threads>
</iq>
```

Field semantics:

- `channel` — bare JID of the MUC room or DM partner the thread lives in.
- `thread-id` — XEP-0201 thread ID.
- `last-stanza-id` — XEP-0359 stanza-id of the most recent message in the thread; the client uses this to anchor MAM follow-on fetches.
- `last-activity` — RFC 3339 timestamp of the most recent message.
- `unread` — count of unread messages in this thread for this user.
- `reply-count` — total messages in the thread (root + replies).
- `has-unread` — convenience boolean, equivalent to `unread > 0`. Server-set so the client doesn't have to recompute.
- `<root-author>` — bare JID of the user who started the thread.
- `<preview>` — short preview of the most-recent message in the thread (already truncated server-side, same rules as inbox previews).
- `<thread-title>` — display title for the thread, if one has been set.

Ordering is `last-activity DESC` over the entire result set; the two-section split (Unread / Following) is a pure client-side partition of the returned list using `has-unread`.

ACL: server returns `<forbidden/>` if the requesting full JID's bare doesn't match the user.

Disco: `urn:waddle:threads:0` advertised on the user's server in disco#info, alongside the existing `urn:xmpp:inbox:0`.

### Audit-doc correction (replaces the original "inbox cleanup")

`docs/xep-conformance-audit.md` lists XEP-0430 with namespace `urn:xmpp:inbox:0`. The implementation actually uses `urn:waddle:inbox:0` and `erlang-solutions.com:xmpp:inbox:0`. The audit row gets corrected — namespace updated, status note added explaining the two surfaces. No code changes.

### Disco changes

- Add: `urn:waddle:threads:0` on the user-server's disco#info features.
- No change to advertised features for `urn:xmpp:inbox:0` (still advertised, wire just becomes spec-clean).

## Server architecture

### Data source

The existing `inbox_entries` table is the source of truth. Schema relevant to this work (already in place from prior PRs):

```sql
inbox_entries (
  user_jid TEXT,
  partner_jid TEXT,
  thread_id TEXT NOT NULL DEFAULT '',
  kind TEXT,
  last_stanza_id TEXT,
  last_updated INTEGER,
  unread INTEGER,
  preview TEXT,
  thread_title TEXT,
  reply_count INTEGER,
  author TEXT,
  PRIMARY KEY (user_jid, partner_jid, thread_id)
)
INDEX idx_inbox_entries_user_room_threads (user_jid, partner_jid, thread_id) WHERE thread_id != ''
```

The threads query reads rows where `user_jid = me AND thread_id != ''`, ordered by `last_updated DESC`, RSM-paginated.

**V1 simplification of "participated or unread":** the inbox writes a row for every thread the user has *seen messages in* (sender or recipient — XEP-0430 inbox semantics), so the V1 query returns "every thread you've been exposed to, plus any with current unread." This is wider than Slack's "followed-or-unread" (Slack tracks explicit follow). It's the right behavior to ship first because the data is already there and the noise floor is low for a normal user. V2 can introduce explicit follow/unfollow and a `participated` filter knob if real usage proves the list is too noisy.

### Module layout

New module `server/crates/waddle-server/src/threads/`:

- `mod.rs` — public entry points
- `query.rs` — typed `ThreadsQuery` request struct, `ThreadsPage` response struct, builders
- `storage.rs` — DB read against `inbox_entries`
- `wire.rs` — typed XML builders for `<threads>` / `<thread>` elements per XEP-0050 / XEP-0059 / RFC 3339 conventions (all via `minidom::Element`, no `format!` for XML)

IQ handler registered alongside the existing inbox IQ handler.

### ACL

Bare-JID match on the requesting full JID against the addressed JID. Same pattern as the inbox handler.

### Tests (dedicated XEP test file per the audit hard rule)

`server/crates/waddle-server/tests/waddle_threads_query_ws.rs` covers:

- Empty result on a fresh account.
- Single channel, multiple threads — returns all, sorted by recency.
- Pagination: page size 2, three results, cursor round-trips.
- ACL: querying another user's threads returns `<forbidden/>`.
- Unread counts match what the existing Waddle inbox reports for the parent conversation.

No changes to existing inbox tests.

## Client architecture

### Sidebar entry

A new top-level item "Threads" in the main left sidebar, between Inbox and the channel list. Badge shows `unread-threads` from the most recent `<threads>` response. Lives in the existing nav component — find the inbox/DM entries and follow the pattern.

### Routing

New Astro page: `chat/src/pages/threads.astro`. Mounts a Vue island with `ThreadsListPanel.vue`.

### Components

- `chat/src/components/chat/ThreadsListPanel.vue` — outer shell. Fetches the first page on mount, renders two `<section>` blocks ("Unread", "Following"), handles infinite scroll via RSM cursor.
- `chat/src/components/chat/ThreadsListRow.vue` — single thread row. Title, channel chip, recency, unread badge, click handler to open the thread.
- No new design tokens — reuse existing `chat-thread-chip`-adjacent styles where they apply, otherwise inbox-row styles. Match the visual weight of the inbox list.

### Wasm boundary

`server/crates/waddle-xmpp-client/src/xep/threads.rs` (or sibling module):

- Typed `ThreadEntry`, `ThreadsPage` Rust structs.
- `build_fetch_threads_iq(after_cursor: Option<&str>, page_size: Option<u32>) -> Element`.
- `parse_threads_response(iq: &Element) -> Option<ThreadsPage>`.

`server/crates/waddle-xmpp-client-wasm/`:

- `fetch_threads(opts: JsValue) -> Promise<ThreadsPage>` (mirrors `fetch_inbox` pattern).

TS-side type in `chat/src/lib/xmpp/wasm-types.ts`:

```ts
export interface WasmThreadEntry {
  channel: string;
  thread_id: string;
  last_stanza_id: string;
  last_activity: string;        // RFC 3339
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

### Live updates

When the inbox storage writes a row for a non-empty `thread_id`, emit a `WasmThreadsDelta` event the chat client subscribes to. The Threads view applies the delta in place rather than re-fetching the whole page. Implementation hook: the existing inbox-row-update event path (used by the inbox view today) — add a thread-flavored branch.

Pragmatic V1: if a clean event-emission hook isn't easy, fall back to refetching the first page on inbox updates. Note as a TODO in the PR; do not block on it.

### Tests

`chat/src/components/chat/__tests__/ThreadsListPanel.test.ts`:

- Renders empty state when no entries.
- Splits Unread (top) and Following (bottom) correctly.
- Click on a row invokes thread-open with `(channel, thread_id)`.

## Implementation order (PR commits, conventional + scoped)

1. `feat(server): urn:waddle:threads:0 query module`
2. `test(server): waddle_threads_query_ws`
3. `feat(server): wasm bindings for fetch_threads`
4. `feat(chat): Threads sidebar nav entry`
5. `feat(chat): ThreadsListPanel with Unread/Following sections`
6. `feat(chat): live thread-delta updates from inbox writes` *(may collapse into 5 if event hook is trivial; or note as a follow-up TODO)*
7. `test(chat): ThreadsListPanel`
8. `docs(server): conformance audit — urn:waddle:threads:0 row + XEP-0430 row correction`

## Risks and open questions

- **Inbox event hook for live deltas** — depending on what the current event path looks like, this may be a 5-minute change or a small refactor. Plan allows it to be a TODO if it bloats the PR.
- **Existing inbox tests** — the cleanup will require updating any test that asserted the presence of the Waddle extras. Mechanical, but it's a chunk of test diff.
- **Empty-state copy** — what do we say when a user has no threads? "Threads you've replied in or have new messages will show up here." Fine but worth a look during implementation review.

## Out of scope (V2 candidates)

- Filter toggle: Unread only / Following / All in channels I've joined.
- Thread mute/pin/archive.
- Per-channel Threads tab inside a channel view (mockup A from the brainstorm — viable later if global view isn't enough).
- Cross-device PEP push for thread state (the data is already server-side; this is only relevant if we ever need offline pre-cached thread lists).

## Queued follow-up: migrate Waddle inbox to XEP-0430

During this brainstorm we identified that Waddle's conversation-level inbox runs on a Waddle-private `urn:waddle:inbox:0` IQ shape (plus an ESL-compat `erlang-solutions.com:xmpp:inbox:0` surface), rather than XEP-0430's `urn:xmpp:inbox:0` / `urn:xmpp:inbox:1`. XEP-0430 is Deferred status (not Draft/Active), but it's the standards-track direction.

Migrating is a real refactor — XEP-0430 streams forwarded `<message>` stanzas with a final `<fin/>` IQ rather than returning an inline list — so it gets its own PR. Threads ships independently because the threads view is a separate Waddle-namespaced query that doesn't depend on which inbox shape we run.

Decision deferred to that PR: keep ESL-compat surface or drop it. Tracking: separate audit-doc row + future PR.
