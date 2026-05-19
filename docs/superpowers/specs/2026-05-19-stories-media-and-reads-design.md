# Stories: Instagram-like media composer + per-user read state

Status: draft (rev 2, post-SME review)
Date: 2026-05-19

## Motivation

Today the Stories composer (`chat/src/components/community/StoriesPane.vue`)
asks for a media URL string. Users have no way to attach a file, take a
photo, or record a clip in-app, and there is no notion of "seen" — every
story renders with the same primary-coloured avatar ring forever.

This spec covers two paired changes that together make Stories feel
Instagram-like:

1. **Media composer overhaul** — replace the URL field with file attach,
   in-app photo capture, and in-app short video recording, all uploaded
   via the existing XEP-0363 HTTP File Upload pipeline.
2. **Per-user read state** — track which stories a user has opened, keep
   that state in a private PEP node so it syncs across devices, and dim
   read stories in the rail.

The two are landed together because they share the same surface
(`StoriesPane.vue`) and the same goal (familiar Instagram-style UX) —
splitting them would force a second pass over the same component within
days.

## Non-goals

- Filters, stickers, text overlays on captured media
- Multi-clip stitching or trimming UI
- Story view counts visible to authors (separate feature)
- Replacing the existing XEP-0501 stories transport
- Encrypting story media (stories are community-broadcast; consistent
  with the existing plaintext model — the OMEMO `encrypted-attachments`
  path is for DMs)
- Migrating any persisted user data — there is none today

## Architecture

### Layering

```
StoriesPane.vue                    (rail, reader, dims read stories)
   ├── StoryComposer.vue           (NEW: file/photo/video capture + preview)
   └── (read-store consumed)
            ↓
useStories() composable            (existing; gains read-store wiring)
            ↓
BrowserXmppClient
   ├── publishStory()              (existing)
   ├── publishStoryReads()         (NEW)
   └── fetchStoryReads()           (NEW)
            ↓
waddle-xmpp-client-wasm
   └── client_story_reads.rs       (NEW: PEP publish/fetch IQ builders + typed forms)
            ↓
waddle-xmpp-core
   └── waddle_story_reads.rs       (NEW: typed Reads / ReadEntry / StoryId, namespace const)
            ↓
waddle-server pubsub_fanout.rs     (PATCHED: private-PEP carve-out gains story-reads node)
```

The composer is split out of `StoriesPane.vue` so the rail/reader
component stays small and focused. The read-store is an interface so the
UI never reaches into XMPP details directly.

### Storage shape — read state

A single PEP item on the user's own JID, conforming to XEP-0223
"Persistent Storage of Private Data via PubSub". Node options
(committed to via `<publish-options>` precondition form on every
publish, per XEP-0060 §7.1.5):

- `pubsub#persist_items = true`
- `pubsub#access_model = whitelist` (private — only the owner can fetch
  via items request)
- `pubsub#send_last_published_item = never`
- `pubsub#max_items = 1`

All four MUST appear in the publish-options precondition. Omitting any
of them lets a server auto-create with different defaults and silently
diverge (e.g. unbounded `max_items`, `on_sub_and_presence` leaking to
roster contacts).

- **Node:** `urn:waddle:story:reads:0`
- **Item id:** the constant string `current` (overwrite-in-place)
- **Payload namespace:** `urn:waddle:story:reads:0` (custom — no XEP
  defines a story-read shape; per the CLAUDE.md hard rule, `urn:waddle:*`
  is correct here). The node id and namespace coincide deliberately,
  matching the XEP-0501 / XEP-0163 one-node-per-namespace convention.
- **Payload XML:**

  ```xml
  <reads xmlns="urn:waddle:story:reads:0">
    <read id="story-uuid-1" at="2026-05-19T10:11:12Z"/>
    <read id="story-uuid-2" at="2026-05-19T10:13:44Z"/>
  </reads>
  ```

- **`id`** is the XEP-0501 pubsub item id of the story (e.g.
  `story-<uuid>`) — globally unique across communities, so we do not need
  to scope by community JID.
- **`at`** is the RFC 3339 timestamp the client first marked the story
  read. Used only for pruning.
- **Pruning:** on every publish, drop entries older than 48h. Also cap
  total entries at 5000 as a defence in depth. Stories themselves expire
  at 24h; 48h buys a safety margin against clock skew and the local
  1-minute tick.

### Security & fan-out (XEP-0223 §Security)

1. **`from` validation (mandatory):** `fetchStoryReads()` parses only IQ
   results whose `from` attribute is absent or equal to the account's
   bare JID. Any future pubsub event handler for this node MUST drop
   events with a `from` not equal to the account's bare JID. Per
   XEP-0223 v1.1.1 Security Considerations (CVE-2023-28686) — a
   malicious contact can otherwise spoof read-state injection.
2. **Server-side fan-out carve-out (mandatory):** Whitelist access
   model on a PEP node controls **items fetch**, not notification
   fan-out. `server/crates/waddle-server/src/pubsub/pubsub_fanout.rs`
   currently exempts only the bookmarks node (`is_private_bookmarks_node`
   check). The story-reads node MUST be added to that exemption (or the
   check generalised to "PEP node with `access_model = Whitelist`")
   before this feature ships. Without this, every roster contact whose
   client advertises `urn:waddle:story:reads:0+notify` would receive
   the read-state payload as a headline event.
3. **Filtered notifications opt-in (XEP-0163):** The Waddle client MUST
   add `urn:waddle:story:reads:0+notify` to its disco features (XEP-0115
   caps) for same-account cross-device delivery to ever work. The server
   already advertises `filtered-notifications`. (Cross-device live sync
   is deferred to a follow-up; the caps entry can land with v1 so the
   plumbing is ready.)

### Read flow

1. On `useStories()` mount (after the first successful `refresh()`),
   call `fetchStoryReads()` once. Result populates an in-memory
   `Set<string>` of read story ids. On failure, the set stays empty
   and no error is surfaced to the user — read state is non-critical.
2. `selectStory(index)` calls `markRead(story.id)`:
   - Adds id to the in-memory set immediately (UI updates).
   - Debounces a `publishStoryReads()` call (250 ms) so opening N stories
     in quick succession coalesces into one PEP publish.
3. **Auto-mark own stories:** when `publishStory()` succeeds, the
   composable calls `markRead(returnedStory.id)` so a user never sees
   their own story as unread on any device. (XEP-0501 defines no
   server-side read affordance; this is the canonical fix.)
4. UI: `StoriesPane.vue` swaps the avatar `ring-primary/70 ring-offset-1`
   class for `ring-muted/50 ring-offset-1` when the id is in the set.
5. Cross-device sync: v1 does not subscribe to the node; opening the
   same story twice across devices is a benign double-publish. The
   `+notify` caps entry is registered for future live sync.

### Media composer

`StoryComposer.vue` is mounted with Astro `client:only="vue"` (the
component touches `navigator.mediaDevices` which does not exist in the
Cloudflare Workers SSR build). All media-API access is gated behind
`onMounted` / explicit user interaction; no top-level module evaluation
references `navigator`.

It exposes one prop (`busy: boolean`) and emits one event
(`submit: { body?: string, file?: File | Blob, mediaKind?: 'image' | 'video' }`).
Internally it has three modes selected by a tab bar:

| Mode    | Behaviour                                                                  |
| ------- | -------------------------------------------------------------------------- |
| Attach  | `<input type="file" accept="image/*,video/*">`; previewed inline.          |
| Photo   | `getUserMedia({ video: { facingMode: 'user' } })` → live `<video playsinline muted autoplay>`; shutter → `canvas.toBlob('image/jpeg', 0.92)`. Flip button toggles to `facingMode: 'environment'`. |
| Record  | Same stream; `MediaRecorder` with `videoBitsPerSecond: 2_000_000` and the first-supported MIME from `['video/webm;codecs=vp9,opus', 'video/webm;codecs=vp8,opus']`; if none, construct `MediaRecorder(stream)` with **no `mimeType` option** (Safari path — Safari rejects `video/mp4` as a recordable type and only works when allowed to pick). Tap-to-start, tap-to-stop, hard cap **20s**, on-screen countdown. |

**iOS Safari requirements:** the live preview `<video>` element MUST
have `playsinline`, `muted`, and `autoplay` attributes. Without
`playsinline`, iOS fullscreens the stream and the composer becomes
unusable.

**Lazy permission request:** `getUserMedia` is called **only** when the
user activates the Photo or Record tab — never on composer mount, and
never on the Attach tab. This avoids the dark-pattern of asking for
camera access before the user signals intent to capture.

**Camera track lifecycle (mandatory):** the `MediaStream` is stored in a
ref. `stream.getTracks().forEach(t => t.stop())` MUST run in every one
of these places: (a) `onBeforeUnmount`, (b) the composer's Cancel /
close handler, (c) tab-switch out of Photo/Record into Attach,
(d) replacement of one stream with another (camera flip). Object URL
revocation alone does NOT stop the camera track — the hardware indicator
light stays on until `.stop()` runs. The component owns a single
`disposeCamera()` function that's invoked from all four sites.

**Object URL lifecycle:** preview URLs are held in a ref. A `watch` on
that ref calls `URL.revokeObjectURL(prev)` before assigning a new value;
`onBeforeUnmount` revokes the current value. The same revocation runs
on tab switch and on Cancel.

**Pre-upload size check:** when a Photo/Record completes, the composer
checks `blob.size > MAX_FILE_UPLOAD_BYTES` **before** requesting a slot,
so the user sees the error inline with the blob still in memory and can
re-shoot. XEP-0363 PUT is not resumable; failing late costs the user
their clip.

**Sanitised filenames:** the upload layer replaces user-facing filenames
with `story-<uuid>.<ext>` before calling `request_upload_slot`. This
keeps PII (e.g. `vacation-photo-alice-birthday.jpg`) out of XEP-0363
GET URLs and out of Faro OTel spans on `XMLHttpRequest`.

`StoriesPane.vue` listens for `submit`, calls `uploadFile()` against the
discovered XEP-0363 service, and then `publishStory({ body, mediaUrl: slot.getUrl })`.
The 10 MB `MAX_FILE_UPLOAD_BYTES` cap is reused. At 2 Mbps cap and 20 s
duration the worst-case video payload is ≈5 MB before audio overhead —
well inside the cap.

Errors surfaced inline above the composer:

- `NotAllowedError` → "Allow camera access in your browser to capture
  photos or video."
- `NotFoundError` → "No camera detected."
- `NotSupportedError` from `new MediaRecorder()` → hide the Record tab,
  show "Recording isn't supported on this browser."
- `blob.size > MAX_FILE_UPLOAD_BYTES` → "That recording is too large
  (over 10 MB). Try a shorter clip." (shown before upload starts)
- Upload `413` or upload network error → "Couldn't upload — please try
  again."

## Typed payloads (server side)

Per the project hard rule, no `String`/`&str` blobs at boundaries.
`waddle-xmpp-core::waddle_story_reads` exposes:

```rust
pub const NS_WADDLE_STORY_READS: &str = "urn:waddle:story:reads:0";
/// Coincides with NS_WADDLE_STORY_READS by XEP-0163 one-node-per-namespace
/// convention; kept as a separate name for call-site readability.
pub const PEP_NODE_WADDLE_STORY_READS: &str = NS_WADDLE_STORY_READS;
pub const PEP_ITEM_WADDLE_STORY_READS: &str = "current";
pub const READ_ENTRY_MAX: usize = 5000;

/// Validated wrapper around an XEP-0501 pubsub item id (e.g. "story-<uuid>").
/// Empty strings are rejected at construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoryId(String);

impl StoryId {
    pub fn new(id: impl Into<String>) -> Result<Self, StoryReadsParseError> { /* ... */ }
    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryReadEntry {
    pub story_id: StoryId,
    pub at: DateTime<Utc>,
}

/// Use BTreeSet keyed by `story_id` to enforce uniqueness structurally:
/// `prune_before` semantics stay obvious, and a duplicate re-mark just
/// updates the `at` (via remove-then-insert in `mark_read`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryReads {
    entries: std::collections::BTreeMap<StoryId, DateTime<Utc>>,
}

impl StoryReads {
    pub fn build_element(&self) -> minidom::Element { /* typed minidom builder, NOT format! */ }
    pub fn parse(el: &minidom::Element) -> Result<Self, StoryReadsParseError> { /* ... */ }
    pub fn prune_before(&mut self, cutoff: DateTime<Utc>);
    pub fn cap_to(&mut self, max: usize); // drops oldest by `at` until len <= max
    pub fn mark_read(&mut self, id: StoryId, at: DateTime<Utc>);
    pub fn contains(&self, id: &StoryId) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = (&StoryId, &DateTime<Utc>)>;
}

#[derive(Debug, thiserror::Error)]
pub enum StoryReadsParseError {
    #[error("wrong element name: expected `reads`, got `{0}`")]
    WrongElementName(String),
    #[error("wrong namespace: expected `{}`, got `{0}`", NS_WADDLE_STORY_READS)]
    WrongNamespace(String),
    #[error("<read> missing `id` attribute")]
    MissingId,
    #[error("<read> has empty `id` attribute")]
    EmptyId,
    #[error("<read> missing `at` attribute")]
    MissingAt,
    #[error("<read> `at` is not RFC 3339: {0}")]
    BadTimestamp(String),
}
```

**Publish-options form** (the wasm bridge constructs this with typed
`DataForm` / `Field` builders from `waddle_xmpp_core::dataform`, never
with `format!`):

```
<x xmlns="jabber:x:data" type="submit">
  <field var="FORM_TYPE" type="hidden">
    <value>http://jabber.org/protocol/pubsub#publish-options</value>
  </field>
  <field var="pubsub#persist_items"><value>true</value></field>
  <field var="pubsub#access_model"><value>whitelist</value></field>
  <field var="pubsub#send_last_published_item"><value>never</value></field>
  <field var="pubsub#max_items"><value>1</value></field>
</x>
```

The wasm bridge (`client_story_reads.rs`) returns `JsStoryReads`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsStoryRead {
    pub id: String,        // StoryId.0 — narrows to plain string only at JS boundary
    pub at: String,        // RFC 3339, like JsStory::posted/expires
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsStoryReads {
    pub entries: Vec<JsStoryRead>,
}

impl From<StoryReads> for JsStoryReads { /* iter + map */ }
```

The `StoryId` newtype is unwrapped to `String` only at the JS boundary —
mirroring how `client_stories.rs::JsStory` serialises typed timestamps
as RFC 3339 strings. Inside Rust the newtype is preserved.

## Testing

### Rust unit tests (`waddle-xmpp-core`)

`server/crates/waddle-xmpp-core/tests/waddle_story_reads.rs`:

- `parse_round_trip` — build → serialise → parse yields equal struct
- `parse_rejects_missing_id` — `<read>` without `id` → `MissingId`
- `parse_rejects_empty_id` — `<read id="">` → `EmptyId`
- `parse_rejects_bad_timestamp` — non-RFC3339 `at` → `BadTimestamp`
- `parse_rejects_wrong_element` — `<readz>` → `WrongElementName`
- `parse_rejects_wrong_namespace` — wrong xmlns → `WrongNamespace`
- `parse_ignores_unknown_children` — forward-compat
- `parse_ignores_unknown_attrs` — `<read id at device="X">` parses
- `prune_before_drops_old_entries` — entries with `at < cutoff` are
  dropped, newer kept
- `cap_to_drops_oldest_first` — at capacity, oldest `at` is dropped
- `mark_read_updates_existing` — re-marking same id updates `at`
- `story_id_rejects_empty` — `StoryId::new("")` errors out

### Rust integration tests (`waddle-server`)

`server/crates/waddle-server/tests/xep0223_story_reads_ws.rs`:

- `publish_then_fetch_returns_entries` — publish via wasm client, fetch
  via raw IQ, assert equality
- `republish_overwrites_item` — second publish replaces (item id stays
  `current`, max_items=1 enforced)
- `publish_iq_carries_required_publish_options` — sniff the published
  IQ; assert `<publish-options>` contains all four required fields with
  expected values
- `node_is_private_to_owner` — a second account fetching the first
  account's node receives `<error type="auth"><forbidden/>` (the
  in-tree server's documented Whitelist behaviour; if implementation
  diverges, the test will tell us in CI before merge)
- **`private_pep_does_not_fan_out_to_roster`** — second account is on
  the publisher's roster and advertises `urn:waddle:story:reads:0+notify`
  in caps; assert it receives NO message stanza when the publisher
  publishes. This proves the `pubsub_fanout.rs` carve-out is wired.

### TypeScript tests

- `StoryComposer.vue` (vitest + happy-dom):
  - mode switching, preview render, MIME-detection branches (vp9 →
    vp8 → no-mime fallback)
  - pre-upload size check fires when `blob.size > MAX`
  - `getUserMedia` stubbed via `vi.spyOn(navigator.mediaDevices, 'getUserMedia')`
  - **camera lifecycle**: track `.stop()` is called on unmount, on
    Cancel, on tab switch, and on camera flip (assert via mocked
    MediaStream with tracked `.stop()` spy)
  - `facingMode` toggle test
- `services/stories.ts`:
  - `markRead` adds id locally, then triggers exactly one publish after
    debounce window (fake timers)
  - publishing your own story triggers an auto `markRead`
  - `fetchStoryReads` failure leaves the set empty and surfaces no
    user-facing error

### Manual

- iOS Safari 17+: camera permission prompt on Photo tab activation;
  live preview renders inline (not fullscreen); record + upload + publish
- Chrome desktop: photo, then record, then attach, all in one session;
  camera light goes off on Cancel
- Open story on device A, open the chat on device B, story shows
  dimmed ring on next mount
- Confirm camera indicator is off after closing the composer mid-record

## Build order

1. **Server foundation** — `waddle-xmpp-core::waddle_story_reads`
   (typed `StoryId`, `StoryReads`, `StoryReadsParseError`) + Rust unit
   tests
2. **Server fan-out fix** — extend `pubsub_fanout.rs`
   `is_private_bookmarks_node` to also exempt `urn:waddle:story:reads:0`
   (and rename the predicate to `is_private_pep_node`); regression test
   for bookmarks behaviour
3. **Wasm bridge** — `client_story_reads.rs` with `story_reads_publish`
   / `story_reads_fetch`, typed `<publish-options>` builder, integration
   tests on `waddle-server` (including the fan-out test)
4. **Chat: caps + read-store** — add `urn:waddle:story:reads:0+notify`
   to the client's disco features; `StoryReadStore` interface +
   `PepStoryReadStore` implementation; wire into `useStories`; update
   `StoriesPane.vue` ring class; clean up `composerMediaUrl` and related
   refs in `StoriesPane.vue` so knip stays clean
5. **Chat: media composer** — `StoryComposer.vue` with three modes,
   replace the URL input
6. **Adversarial review + CI green**

Steps 1–3 are independent of 4–5 only in code; the user-facing change
only makes sense once both ship, so this lands as a single PR.

## Risks

- **Safari MediaRecorder support** — Safari ≥14 supports `MediaRecorder`
  but rejects most explicit `mimeType` values. Mitigation: omit
  `mimeType`. If `new MediaRecorder(stream)` itself throws, hide the
  Record tab.
- **PEP node creation** — XEP-0223 says the server SHOULD auto-create on
  first publish-with-options. The in-tree server's PEP module is
  expected to honour `<publish-options>` precondition fields. The
  integration test `publish_iq_carries_required_publish_options` and
  `publish_then_fetch_returns_entries` together prove this end-to-end.
- **Large recordings** — duration capped at 20s, bitrate capped at
  2 Mbps. Pre-upload size check rejects oversize blobs before the slot
  request.
- **Roster fan-out regression** — the existing bookmarks carve-out in
  `pubsub_fanout.rs` is the only thing keeping XEP-0402 bookmarks
  private today. We're extending that mechanism; the regression test
  for bookmarks must continue to pass.
