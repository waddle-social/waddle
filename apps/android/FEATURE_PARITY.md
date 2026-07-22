# Feature parity: Waddle web app vs. Android app

## Context

The web app (`chat/`, Astro + Vue) and the Android app (`apps/android/`, Kotlin +
Compose) are both thin UIs over the same Rust XMPP core
(`server/crates/waddle-xmpp-client*`): the web app consumes it through WASM, the
Android app through UniFFI bindings (`server/crates/waddle-xmpp-client-ffi`,
generated into `core/client/src/main/kotlin/social/waddle/client/ffi/`). Because
the protocol layer is shared, many gaps below are UI-only: the FFI already
carries the typed payloads (mentions, markup spans, link previews, call
signaling) and Android simply does not render or drive them yet. This document
compares user-facing capability per area, verified against both codebases as of
2026-07-16.

## Legend

| Symbol | Meaning |
| --- | --- |
| ✅ | Full parity — feature works end to end on this client |
| 🟡 | Partial — data model or reduced form exists, but incomplete UI/behavior |
| ❌ | Missing on this client |
| 🚫 | Intentionally out of scope on this client (reason in Notes) |

## Core messaging

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Channel (MUC) messaging | 0045 | ✅ | ✅ | `app/src/main/kotlin/social/waddle/android/feature/channel/ChannelScreen.kt` |
| 1:1 direct messages | — | ✅ | ✅ | `app/src/main/kotlin/social/waddle/android/feature/dm/DmScreen.kt` |
| Group DMs (multi-party) | 0045 | ✅ | ❌ | Web: `chat/src/lib/xmpp/group-dm.ts`; Android has only 1:1 DM + MUC |
| History (MAM) | 0313 | ✅ | ✅ | Android: `fetch_room_history`/`fetch_dm_history` FFI verbs |
| Message search (MAM full-text) | 0313 | ✅ | ✅ | Android: `search_room_history`/`search_dm_history` FFI verbs + `feature/search/MessageSearchSheet.kt`, entry in the conversation top bar |
| Corrections | 0308 | ✅ | ✅ | Edit action in `MessageActionSheet.kt` |
| Retraction | 0424 | ✅ | ✅ | `send_retraction` FFI verb |
| Reactions | 0444 | ✅ | ✅ | Emoji reactions on both |
| Replies | 0461 | ✅ | ✅ | Android sends 0461 reply metadata with the XEP-0428 fallback prefix (`buildReplyFallbackPrefix`, web parity) |
| Threads | 0201 | ✅ | ✅ | Android has dedicated `ThreadScreen.kt`, thread overview list, per-message reply counts |
| Delivery acks / receipts | 0184, 0198 | ✅ | ✅ | Ack-driven delivery states on both |
| Offline outbound queue | — | ✅ | ✅ | Android: `core/client/.../client/OutboundQueue.kt` |
| Typing / chat states | 0085 | ✅ | ✅ | Android: `ComposerTypingNotifier.kt` + `TypingIndicator.kt` |
| Read markers + sync | 0333, 0490 | ✅ | ✅ | Android publishes/fetches/subscribes MDS, local `ReadCursorStore.kt` |
| End-to-end encryption (OMEMO) | 0384 | ❌ | ❌ | Missing on BOTH clients — not an Android parity gap; code comments confirm plaintext |

## Conversation features

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Pinned messages | urn:waddle:pin:0 | ✅ | ✅ | Android has pin/unpin actions + `PinStore.kt`; web adds `PinnedPanel.vue` |
| @-mentions | 0372 | ✅ | ✅ | Composer autocomplete (`MentionPopover`/`MentionSpanTracker`) sends 0372 references; rendered + self-mention highlight in `RichMessageBody.kt` |
| Mention/nick colors | 0392 | ✅ | ✅ | Shared Rust hue via `consistent_color_hue` FFI; `theme/ConsistentColor.kt` |
| Per-conversation notify modes / mute | 0492 | ✅ | ✅ | Android: `NotifySettingsStore.kt` + notify sheet in the conversation top bar; `NotificationPolicy.kt` enforces never/on-mention/always |
| Bookmarks | 0402 | ✅ | ❌ | Android channel list comes from Waddle `discover_topology`, not 0402 |
| Inbox / unread overview | 0430 | ✅ | 🟡 | Android computes unread locally (`UnreadStore.kt`) from live traffic; no 0430 sync |
| Slash commands | — | ✅ | ❌ | Web: `chat/src/lib/slash-dispatch.ts` |
| Room create / configure | 0045 | ✅ | ✅ | Android: create-channel dialog (owner-gated) + owner settings sheet over `create_room`/`fetch_room_config`/`submit_room_config`/`destroy_room` (§10 GET-merge-SET in Rust). Space intents deferred (need XEP-0060 spaces-node builders) |

## Presence & profile

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Presence & status | RFC 6121 | ✅ | ✅ | Android: `PresenceStore.kt` |
| Avatars | 0084 | ✅ | ✅ | Publish/remove via `publish_avatar`/`disable_avatar` (photo picker → ≤512² PNG); SHA-1-keyed cache honors §4.2 no-refetch. XEP-0153 vCard-temp hash publish is not implemented on any client — follow-up |
| vCard / profile editing | 0292 | ✅ | ✅ | `feature/profile/ProfileScreen.kt` — optimistic save/rollback over `publish_vcard4` (web VCardEditor parity) |
| Rich presence (mood/activity/tune) | 0107, 0108, 0118 | ✅ | ✅ | Mood/activity/tune publish + clear from the profile screen (84-kind closed vocab enforced at the FFI; manual tune submit publishes immediately, web parity) |
| Idle time | 0319 | ✅ | ✅ | DM subtitle “away · idle Nm” (`DmScreen.kt`), minute ticker |

## Notifications

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| New-message notifications | — | ✅ | ✅ | Android: MessagingStyle in `service/MessageNotifier.kt` |
| Inline reply from notification | — | ❌ | ✅ | Android-only (RemoteInput + `ReplyReceiver.kt`); browsers lack an equivalent |
| Push notifications | 0357 | ✅ | 🚫 | Android deliberately uses a foreground service + reconnect instead; 0357 FFI exists but is intentionally unused |
| Notification/sound/read-receipt toggles | — | ✅ | ✅ | Android: `feature/settings/SettingsScreen.kt` (theme, notifications, sounds, read receipts, battery-optimization) |

## Media & rich content

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| File/HTTP upload + attachments | 0363, 0446, 0447 | ✅ | ✅ | Android: `AttachmentUploader.kt`, slot request via FFI |
| Inline image display | 0447 | ✅ | ✅ | Android renders via Coil in `MessageCard.kt` |
| Encrypted file attachments | 0448 | ✅ | ❌ | Web: `chat/src/lib/xmpp/encrypted-attachments.ts`; Android uploads plaintext (no OMEMO) |
| Rich text editor / markup | 0394 | ✅ | ✅ | Rendered via `RichBody.kt`/`RichMessageBody.kt`; composer converts markdown at send (`ComposerMarkdown.kt`) |
| Markdown rendering | — | ✅ | ✅ | Neither client renders markdown from received bodies (0394-only); Android converts typed markdown at send |
| Link previews | urn:waddle:link-preview:0 | ✅ | ✅ | Cards in `LinkPreviewCard.kt`; composer lookup token via `lookup_link_preview` FFI |
| Stickers | 0449 | ✅ | ✅ | Android exceeds web parity: inline sticker image (112dp, body = alt text) plus picker (`StickerPickerSheet.kt`), sticker sends, and user-created packs (`CreateStickerPackSheet.kt` → 0363 upload → PEP publish over the FFI); web renders only |
| GIF picker | — | ✅ | ✅ | `GifPickerSheet.kt` over the server-origin `/api/giphy` proxy contract |

## Calls

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Voice/video calls | 0166, 0353, 0272 | ✅ | ✅ | DM calls + Muji group calls (0272 presence flow, SFU mixer session, in-call roster, retained-call recovery); LiveKit media, CallStyle notifications |
| Call extras (backgrounds, noise filter) | — | ✅ | ❌ | Web-only: MediaPipe background effects, AI noise filter |

## Social & community

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Social feed | 0472 | ✅ | ❌ | Web: `chat/src/components/community/FeedPane.vue` |
| Stories | 0501 | ✅ | ❌ | Web: `StoryComposer.vue` |
| Community events / calendar | — | ✅ | ❌ | Web: `chat/src/lib/xmpp/event-calendar.ts` |
| Extensions / plugins | 0050-based | ✅ | ❌ | Web: extension palette + per-room plugin routes |

## Admin & moderation

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Message moderation (remove others' messages) | 0425 | ✅ | ✅ | Android: "Delete for everyone" in the message action sheet, gated on self-presence owner/admin/moderator, with confirm dialog |
| Affiliation management (kick/ban/roles) | 0045 | ✅ | ✅ | Android: members screen (four-tier §9.5 lists merged with live presence) with promote/demote/remove/ban (§9.1) and true §8.2 kick (role→none), owner/admin-gated; XEP-0055 add-member search |
| Hats (role badges) | 0317 | ✅ | ✅ | Single seniority badge on message rows (`AuthorBadges.kt`, authority stays 0045); hat titles on member rows in the members screen |
| Ad-hoc commands | 0050 | ✅ | 🟡 | Android drives typed 0050 commands (push register/disable, `urn:waddle:admin:*`); no generic command palette/form renderer (follow-up) |
| Server admin pages | — | ✅ | 🟡 | Android ships a reduced-scope "Community admin" users list (V1) in settings, gated on the `is_community_owner` probe; the six-panel web console (spaces/channels CRUD, audit, push-health, settings) stays desktop-only by design — the V2 FFI commands exist for a follow-up |

## Platform

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| OAuth login (device-auth flow) | — | ✅ | ✅ | Android: full device-authorization flow via Custom Tabs (`WaddleAuthApi.kt`) |
| Stream management + resumption | 0198 | ✅ | ✅ | Android persists an SM resume snapshot across process death |
| Reconnect / connection lifecycle | — | ✅ | ✅ | Android: foreground service (`WaddleConnectionService.kt`) with Kotlin-driven backoff |
| Theme (light/dark/system) | — | ✅ | ✅ | Both clients |

## Planned next

Remaining gaps, in priority order:

1. **Group DMs and bookmarks** (0402) — multi-party DMs and server-synced room list.
2. **Inbox sync (0430)** — the Rust client parses `urn:waddle:inbox:0`; expose it over the FFI and feed `UnreadStore`.
3. **Slash commands** — composer tokenizer + dispatch (web `slash-dispatch.ts`).
4. **Encrypted attachments (0448)** — AES-GCM download/decrypt + encrypted upload.
5. **Community surfaces** — feed (0472), stories (0501), events, extensions; lowest urgency, largest scope.