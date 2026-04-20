# Reply & Thread Parity for iOS — Design

**Date:** 2026-04-19
**Status:** Approved, ready for plan
**Owner:** apps/apple + server/crates/waddle-xmpp-client

## Background

The web chat app (Astro + Vue clickdummy at `chat/`) fully renders XEP-0461 replies and XEP-0201 threads. The iOS app parses neither: reply markers arrive but are dropped at the FFI boundary, and thread `parent` attributes are never read. User-visible symptom: incoming replies render as ugly `>`-quote fallback text with no reply chip, and threaded conversations collapse into a flat list.

This spec brings iOS to full parity with the web app for:

- **XEP-0461 Message Replies** (inbound rendering + outbound composition)
- **XEP-0428 Fallback Indication** (strip quoted fallback body on supporting clients)
- **XEP-0201 Threads** including nested/multi-level thread chains

## Goals

- Incoming replies render as a compact chip + clean body (no `>` quotes) on iOS
- User can initiate a reply via swipe-right or long-press context menu on any message
- Threads open in a focused `ThreadPanel` adjacent to the main conversation
- Full nested thread support with breadcrumb navigation
- Wire format identical to web app — receivers on any client see the same stanzas

## Non-goals

- Reactions, edits, retractions, read markers (separate specs)
- Thread search/discovery UI beyond what the ThreadPanel provides
- Backwards-compat shims for pre-existing pseudo-threading; we change the wire format cleanly

## Architecture overview

Four layers, each owning one responsibility:

1. **Rust parse/build** (`server/crates/waddle-xmpp-client`) — typed XEP modules, no string XML
2. **Rust FFI** (`server/crates/waddle-xmpp-client-ffi`) — UniFFI-exposed records
3. **Swift adapter** (`apps/apple/Waddle/RustClient/RustXmppClient.swift`) — maps FFI records to `XMPPMessageEvent`
4. **SwiftUI** (`apps/apple/Waddle/**`) — `ChatStore` state + row/panel views

---

## Section 1 — Inbound path (data model + parsing)

### Rust `InboundMessage` extensions

Fields added in `messaging.rs`:

- `reply_to_id: Option<String>` (already parsed — keep)
- `reply_to_sender: Option<Jid>` (already parsed — keep)
- `parent_thread_id: Option<String>` — parse `<thread parent='X'>` attribute
- `reply_fallback: Option<(u32, u32)>` — `<fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start=.. end=../>` parsed as char-offset range per XEP-0428

### New `xep::reply` module

`server/crates/waddle-xmpp-client/src/xep/reply.rs`:

```rust
pub const NS_REPLY: &str = "urn:xmpp:reply:0";
pub const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

pub struct ReplyMarker { pub to: Jid, pub id: String }
pub struct FallbackRange { pub start: u32, pub end: u32 } // char offsets, end exclusive

pub fn parse_reply(el: &Element) -> Option<ReplyMarker>;
pub fn parse_fallback(el: &Element) -> Option<FallbackRange>;
pub fn build_reply_element(m: &ReplyMarker) -> Element;
pub fn build_fallback_element(r: &FallbackRange) -> Element;
```

### New `xep::thread` module

`server/crates/waddle-xmpp-client/src/xep/thread.rs`:

```rust
pub struct ThreadRef { pub id: String, pub parent: Option<String> }

pub fn parse_thread(el: &Element) -> Option<ThreadRef>;
pub fn build_thread_element(t: &ThreadRef) -> Element;
```

No ad-hoc string literals for namespaces or attribute names at call sites.

### FFI `WaddleMessage` changes

`server/crates/waddle-xmpp-client-ffi/src/lib.rs`:

```rust
pub struct WaddleMessage {
    // ...existing fields...
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,          // full JID string
    pub reply_fallback_start: Option<u32>,        // UniFFI has no tuple support
    pub reply_fallback_end: Option<u32>,
    pub parent_thread_id: Option<String>,
}
```

Populate these in `dispatch_event` from `InboundMessage`. Same fields on `WaddleArchivedMessage` for MAM.

### Swift `XMPPMessageEvent` changes

`apps/apple/Waddle/XMPP/XMPPTypes.swift`:

- `replyFallbackRange: Range<Int>?` — reconstructed from `(start, end)` in both `onMessage` listener and `WaddleMamPage.toXMPPArchivePage()`
- `parentThreadID: String?` — populated (currently hardcoded nil)

Existing `replyToID`, `replyToSender`, `threadID` fields stay.

### Swift `visibleBody` helper

```swift
extension XMPPMessageEvent {
    var visibleBody: String {
        guard let range = replyFallbackRange, let body else { return body ?? "" }
        let s = body as NSString
        let prefix = s.substring(to: min(range.lowerBound, s.length))
        let suffix = s.substring(from: min(range.upperBound, s.length))
        return (prefix + suffix).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
```

Used by `ChatMessageRow` and `ChatReplyChip` in place of raw `body`.

### `ChatReplyChip` view

New SwiftUI view mirroring `chat/src/components/MessageCard.vue` chip:

- Vertical accent bar (4pt, sender color)
- Sender nick (caption, bold) + one-line preview (caption)
- Tap → scroll to source message; if source lives in a thread, opens that thread
- Preview resolved via `ChatStore.messagesByID[replyToID]?.visibleBody ?? "(unknown message)"`

---

## Section 2 — UX + navigation

### Initiating a reply

Two entry points on every message row (DM, channel, thread):

**Swipe-right (leading edge)**: `swipeActions(edge: .leading, allowsFullSwipe: true)` with a single "Reply" action (SF Symbol `arrowshape.turn.up.left`, accent tint).

**Long-press context menu** via `.contextMenu`:
- Reply
- Reply in thread
- Copy text
- Copy link (xmpp: URI)
- Copy message ID (debug builds only)

Both route through `ChatStore.beginReply(to: XMPPMessageEvent, inThread: Bool)`.

### Composer reply-to banner

When `ChatStore.replyDraft != nil`, composer shows a banner above the text field:

- Vertical accent bar (4pt, sender color)
- Sender nick (bold) + truncated one-line body preview
- xmark.circle.fill dismiss button
- ~44pt height, `.regularMaterial` background

On send, `ChatStore.sendMessage` reads `replyDraft`, passes fields to FFI, clears on success.

### ThreadPanel view

- Header: parent message (full card, tap → scroll-to-source in main list)
- Divider labelled "N replies"
- Children: `ChatStore.childrenByThreadID[threadID]` ordered by timestamp ascending
- Embedded `ChatComposer` with thread context pre-wired

### NavigationSplitView integration

`ChatView` becomes `NavigationSplitView`:

- **Sidebar**: waddle/channel list (unchanged)
- **Content**: main message list (unchanged)
- **Detail**: `ThreadPanel` when `ChatStore.activeThreadStack` non-empty

Adaptive presentation:

- `horizontalSizeClass == .compact` (iPhone portrait) → `.sheet`
- `horizontalSizeClass == .regular` (iPad/macOS/landscape) → third column

### Nested threads

`ChatStore.activeThreadStack: [String]` holds the thread chain. Breadcrumb bar above ThreadPanel header:

```
Root › Thread A › Thread B (current)
```

Each segment is a button; tap pops the stack to that level. iPhone sheet uses a nested `NavigationStack`; iPad/macOS uses `.navigationDestination`. Visual cap at 8 levels (ellipsize middle); no depth cap in data.

### Thread chip on root messages

When `childrenByThreadID[message.id]` is non-empty, row renders:

```
💬 3 replies · latest 2m ago
```

Tap → pushes threadID onto `activeThreadStack`, opens panel.

### Tap behavior

| Tap target | Behavior |
|---|---|
| Reply chip | Scroll to source; open owning thread if source is in a thread |
| Thread chip | Open/focus thread panel for that root |
| Breadcrumb segment | Pop thread stack to that level |
| Parent header in ThreadPanel | Scroll main list to parent, keep panel open |

### `ChatStore` state additions

- `@Published var replyDraft: ReplyDraft?`
- `@Published var activeThreadStack: [String] = []`
- `private(set) var childrenByThreadID: [String: [String]] = [:]`
- `private(set) var messagesByID: [String: XMPPMessageEvent]` (added if missing)

Indexes maintained on every incoming message and rebuilt on MAM page load. No new persistence — pure in-memory derived state.

---

## Section 3 — Outbound stanzas + testing + build sequencing

### FFI `SendMessageOptions`

Replaces the flat `thread_id` argument on both `send_chat_message` and `send_groupchat_message`:

```rust
pub struct SendMessageOptions {
    pub thread_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
}

pub async fn send_groupchat_message(
    &self,
    room_jid: String,
    body: String,
    options: SendMessageOptions,
);
```

`send_chat_message` mirrors. Default `SendMessageOptions` has all fields None — existing Swift call sites pass `.init()`.

### Rust send path

`messaging.rs::send_*_message` attaches child elements using the typed builders:

```rust
if let Some(marker) = &opts.reply { msg.append(reply::build_reply_element(marker)); }
if let Some(range) = &opts.fallback { msg.append(reply::build_fallback_element(range)); }
if let Some(thread) = &opts.thread { msg.append(thread::build_thread_element(thread)); }
```

No `format!`, no string concatenation, no inline namespace literals.

### Fallback assembly (Swift)

`ChatStore.sendMessage` assembles outbound body when a reply is active:

```
> @<sender-nick> <one-line-preview>
> <original-body>

<user-typed-body>
```

Swift computes `(start, end)` as char (UnicodeScalar view) offsets covering just the `> `-prefixed quote block plus trailing blank line. Sends both the assembled body and the range to Rust via `SendMessageOptions.reply_fallback_start/end`. Receivers that support XEP-0428 strip the range; non-supporting clients see the quote.

### Testing (XEP custom test-suite rule)

`server/crates/waddle-xmpp-client/tests/xep_0461_replies.rs`:

- `builds_message_with_reply_and_fallback` — serialize outbound, assert exact child elements + namespaces + attributes
- `parses_inbound_reply_with_fallback` — parse raw `<message>`, assert all fields populated
- `fallback_range_is_character_offsets` — body with emoji + combining chars; verify char-count semantics
- `missing_fallback_parses_reply_without_range`
- `reply_roundtrips_via_element` — build → serialize → parse-as-element → parse-as-inbound

`server/crates/waddle-xmpp-client/tests/xep_0201_threads.rs`:

- `builds_groupchat_with_thread_parent`
- `parses_inbound_thread_with_parent`
- `thread_without_parent_has_none_parent`
- `thread_parent_survives_round_trip`

All tests use typed builders/parsers. No Swift unit tests — UniFFI surface covered by Rust suite + manual device smoke.

### Build sequencing (4 vertical slices)

Each slice = single commit, ends with device deploy + smoke before the next begins.

**Slice 1 — Reply inbound rendering**
1. Rust parse reply + fallback on `InboundMessage`
2. Inbound half of `xep_0461_replies.rs`
3. FFI: add reply fields on `WaddleMessage` + `WaddleArchivedMessage`; populate in `dispatch_event`
4. Swift: populate `replyToID`/`replyToSender`/`replyFallbackRange` in `onMessage` + `toXMPPArchivePage`
5. Swift: `ChatReplyChip` view + render in `ChatMessageRow`
6. Swift: `visibleBody` helper applied in row body text
7. Deploy & smoke: incoming replies from web client render with chip, no `>` quote

**Slice 2 — Reply outbound**
1. Rust `xep::reply` module + builders
2. Outbound half of `xep_0461_replies.rs`
3. Rust: extend `send_*_message` to attach reply + fallback
4. FFI: `SendMessageOptions` struct; new method signatures; existing call sites pass `.init()`
5. Swift: swipe-right + long-press context menu on `ChatMessageRow`
6. Swift: `ReplyDraft` model + composer banner + dismiss
7. Swift: `ChatStore.sendMessage` assembles quote body, computes fallback range, threads through FFI
8. Deploy & smoke: iOS-originated reply renders as chip on web; non-supporting clients see `>` quote

**Slice 3 — Thread inbound + panel**
1. Rust parse `<thread parent>` on `InboundMessage`
2. Inbound half of `xep_0201_threads.rs`
3. FFI: `parent_thread_id` on WaddleMessage + WaddleArchivedMessage
4. Swift: `childrenByThreadID` index; maintained on message arrival + MAM load
5. Swift: `ThreadPanel` view; `ChatView` refactored to `NavigationSplitView`
6. Swift: thread chip on root messages; tap pushes onto `activeThreadStack`
7. Deploy & smoke: incoming thread replies cluster in panel; chip shows count

**Slice 4 — Thread outbound + nested**
1. Rust `xep::thread` builder
2. Outbound half of `xep_0201_threads.rs`
3. Rust: attach `<thread>` on send
4. Swift: "Reply in thread" context-menu action wires up `SendMessageOptions.thread_id/parent_thread_id`
5. Swift: breadcrumb bar; nested `NavigationStack` in sheet / `.navigationDestination` in detail column
6. Swift: adaptive sheet vs column via `horizontalSizeClass`
7. Deploy & smoke: full nested threading round-trip between iOS and web

### Risks & mitigations

| Risk | Mitigation |
|---|---|
| UniFFI signature changes break call sites | Single commit per slice touches Rust + Swift together; default `SendMessageOptions()` covers callers that don't care |
| Rust vs Swift char-offset math diverges | Canonical quote assembly done once in Swift; Rust only parses what it gets; tests cover emoji + combining chars |
| NavigationSplitView odd on iPhone landscape | Gate detail column on `.regular` horizontal size class; compact always sheet |
| MAM pages arriving after panel opens | `childrenByThreadID` rebuilt on MAM page load; id-set dedupe on append |
| Breadcrumb stack grows unbounded | Visual cap at 8 levels (ellipsize middle); no data cap |

## Success criteria

- Incoming replies from web render on iOS as chip + clean body (no `>` quote)
- iOS swipe-right and long-press both open composer with reply banner
- iOS-originated replies render as chip on web and as `>` quote on non-supporting clients
- Threads open in `ThreadPanel` adjacent to main list (iPad/macOS) or as sheet (iPhone)
- Nested threads navigable via breadcrumb; depth limited only visually
- Every implemented XEP has its dedicated Rust test suite
- No `format!` or string concatenation used anywhere for XML construction
- No new `String`/`&str` fields on public types carrying structured data
