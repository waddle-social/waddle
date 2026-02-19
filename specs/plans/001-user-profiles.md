# PLAN-001: User Profiles — Display Names & Avatars

**Status:** Built (Steps 0–7 complete, Step 8 stretch deferred)
**Spec:** specs/requirements/001-user-profiles.md
**Created:** 2026-02-19

---

## Decisions (locked)

| # | Decision | Rationale |
|---|----------|-----------|
| OQ-1 | **Same-process DB write (Option B)** — ATProto OAuth is handled in `waddle-server` directly, not a separate Colony service. Profile auto-population writes to `vcard_storage` via `VCardStore` in the same Rust process after token exchange. | Codebase confirms OAuth flow lives in `waddle-server/src/auth/atproto.rs` and `server/routes/device.rs`. No separate Colony Worker exists. |
| OQ-2 | **Login-only refresh** — ATProto profiles refreshed on each login, not periodically. | Simplest approach; avoids background jobs. Can add periodic refresh later. |
| OQ-3 | **`source` column** with values `atproto_auto` / `manual`. | Clear semantics for FR-1.3 guard logic. |
| OQ-4 | **XEP-0054 IQ routing exists** — `handle_vcard_get` and `handle_vcard_set` are fully wired in `connection.rs`. | Verified in codebase. |

## Completed Pre-work

### ✅ Step 0: Fix XEP-0054 data model for EXTVAL + DESC round-trip

**Critical blocker resolved.** The XEP-0054 parser/builder has been extended:

- `VCardPhoto` changed from struct (`{ mime_type, data }`) to enum: `Binary { mime_type, data }` | `External { url }`
- `VCard` struct: added `desc: Option<String>` field (DESC element)
- `parse_vcard_element`: parses EXTVAL photos and DESC field
- `build_vcard_element`: serializes both photo variants and DESC
- `handle_vcard_get`: returns stored raw XML directly (no parse→rebuild lossy cycle)
- `handle_vcard_set`: stores raw XML from IQ (no parse→rebuild); still parses for avatar hash
- Avatar hash computation (`XEP-0153`): only for `Binary` photos; `External` photos skip hashing
- All 17 XEP-0054 unit tests pass (4 new: extval roundtrip, binval roundtrip, desc parsing, extval parsing)

## Build Plan (resequenced per review)

### 1. Add `source` column migration + guard logic
- New migration in `server/crates/waddle-server/src/db/migrations.rs`
- `ALTER TABLE vcard_storage ADD COLUMN source TEXT DEFAULT 'manual'`
- Add `VCardStore::set_vcard_if_auto(jid, xml)` — only writes if source is `atproto_auto` or no row exists
- Update `set_vcard()` to mark `source = 'manual'`
- **Tests:** unit tests for source guard logic

### 2. Add Bluesky profile fetcher
- New module `server/crates/waddle-server/src/auth/profile.rs`
- Function: `fetch_bluesky_profile(did: &str) -> Result<BlueskyProfile, ProfileError>`
- Calls `https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor={did}`
- Returns `{ display_name, avatar_url, description }`
- 2s timeout (NFR-4); failure returns error, never panics
- **Security:** validate avatar URL is HTTPS only
- **Tests:** unit tests with mocked HTTP responses

### 3. Hook auto-population into ATProto login flow
- After successful token exchange in device auth callback
- Resolve DID → `fetch_bluesky_profile` → build vCard XML with FN, PHOTO/EXTVAL, DESC
- Call `VCardStore::set_vcard_if_auto()` (preserves manual edits per FR-1.3)
- Log warning on failure; never block authentication (FR-1.4)
- **Observability:** structured log for success/failure/timeout

### 4. Add vCard transport methods
- Extend `WaddleTransport` in `app/gui/src/composables/useWaddle.ts` with `getVCard(jid)` and `setVCard(xml)`
- Implement in Tauri transport (new Rust backend command)
- Implement in WebSocket transport (raw IQ stanza)

### 5. Create vCard Pinia store + lifecycle wiring
- New `app/gui/src/stores/vcard.ts`
- `Map<string, VCardData>` cache keyed by bare JID
- `fetchVCard(jid)` — single fetch with 5s timeout (NFR-2)
- `fetchBatch(jids)` — max 5 concurrent (NFR-1)
- `getDisplayName(jid)` — priority: vCard FN → roster name → JID localpart
- `getAvatarUrl(jid)` — priority: vCard PHOTO → generated fallback
- Wire: fetch own vCard on connect (FR-2.1), batch fetch contacts after roster load (FR-2.2)
- **Security:** HTML-escape display names before rendering

### 6. Update UI components
- Create `AvatarImage.vue` — lazy-loads photo URL, falls back to JID-hash circle
- Update `RosterView.vue` — display name + avatar from vCard store
- Update `ChatView.vue` — message sender name + avatar
- Update `ConversationListView.vue` — conversation display names + avatars
- Update `SidebarNav.vue` — own profile avatar/name
- **NFR-3:** avatars lazy-loaded; vCard fetch does not block roster rendering

### 7. Build ProfileView.vue
- Accessible from sidebar/settings navigation
- Shows: JID (read-only), editable display name, avatar upload
- Client-side resize: max 256×256, JPEG/PNG, 5MB input limit, 100KB output (NFR-5)
- Saves via XEP-0054 IQ set → refreshes local cache + UI (FR-3.4)
- Add route in `app/gui/src/router/`
- **Security:** sanitize uploaded image; validate file type

### 8. Stretch: XEP-0153 client-side vCard update notifications (FR-4)
- Parse `<x xmlns='vcard-temp:x:update'>` from presence stanzas
- Compare photo hash; re-fetch vCard on mismatch
- Fallback: re-fetch all contact vCards per session (FR-4.2)
- Server already broadcasts XEP-0153 in presence (verified)

## Security Checklist
- [ ] vCard XML stored raw — ensure no script injection via minidom (it doesn't execute)
- [ ] EXTVAL URLs: validate HTTPS-only before storing
- [ ] Display names: HTML-escape in Vue templates (Vue auto-escapes `{{ }}` by default)
- [ ] Avatar uploads: validate MIME type, enforce size limits client-side
- [ ] base64 BINVAL: enforce max 100KB decoded size

## Files Changed

### Step 0 (Pre-work): XEP-0054 EXTVAL + DESC round-trip
- `server/crates/waddle-xmpp/src/xep/xep0054.rs` — VCardPhoto enum (Binary|External), DESC field, new tests
- `server/crates/waddle-xmpp/src/connection.rs` — raw XML storage/retrieval, enum pattern matching

### Step 1: Source column migration + guard logic
- `server/crates/waddle-server/src/db/migrations.rs` — V0010_VCARD_SOURCE migration
- `server/crates/waddle-server/src/vcard.rs` — `set_if_auto()`, `get_source()`, `set()` marks manual; 5 new tests

### Step 2: Bluesky profile fetcher
- `server/crates/waddle-server/src/auth/profile.rs` — NEW: `fetch_bluesky_profile()`, `build_vcard_from_profile()`, XML escaping; 5 tests
- `server/crates/waddle-server/src/auth/mod.rs` — registered `profile` module

### Step 3: ATProto login auto-population
- `server/crates/waddle-server/src/server/routes/device.rs` — `auto_populate_vcard()` function, called after device auth callback
- `server/crates/waddle-server/src/server/routes/auth.rs` — called after regular ATProto callback

### Step 4: vCard transport methods
- `app/gui/src/composables/useWaddle.ts` — `VCardData`/`VCardSetRequest` types, `getVCard`/`setVCard` on WaddleTransport, Tauri transport, browser XMPP transport (IQ get/set), disconnected transport

### Step 5: vCard Pinia store
- `app/gui/src/stores/vcard.ts` — NEW: cache, batch fetch (5 concurrent), display name resolution, avatar resolution, own vCard

### Step 6: UI component updates
- `app/gui/src/components/AvatarImage.vue` — NEW: lazy-loaded avatar with JID-hash fallback
- `app/gui/src/views/RosterView.vue` — uses vCard display names + AvatarImage
- `app/gui/src/views/ChatView.vue` — uses vCard display names + AvatarImage for messages
- `app/gui/src/components/SidebarNav.vue` — conversation avatars + display names from vCard, Profile nav link

### Step 7: Profile editing
- `app/gui/src/views/ProfileView.vue` — NEW: edit display name + avatar, client-side resize, save via XEP-0054
- `app/gui/src/router/index.ts` — `/profile` route

### Step 8: XEP-0153 (stretch, deferred)
- Not implemented — server already broadcasts avatar hashes in presence; client-side parsing can be added later
