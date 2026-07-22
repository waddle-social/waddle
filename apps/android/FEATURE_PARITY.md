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
| Message search (MAM full-text) | 0313 | ✅ | ❌ | Web: `chat/src/components/chat/MessageSearchPanel.vue`; no Android UI or FFI query — in flight |
| Corrections | 0308 | ✅ | ✅ | Edit action in `MessageActionSheet.kt` |
| Retraction | 0424 | ✅ | ✅ | `send_retraction` FFI verb |
| Reactions | 0444 | ✅ | ✅ | Emoji reactions on both |
| Replies | 0461 | ✅ | ✅ | Android sends 0461 reply metadata; no explicit 0428 fallback body |
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
| @-mentions | 0372 | ✅ | 🟡 | FFI carries `references`/`mention_uris`; no Android compose or render UI — in flight |
| Mention/nick colors | 0392 | ✅ | ❌ | Web: `consistentColor` in `chat/src/lib/chat-ui.ts` |
| Per-conversation notify modes / mute | 0492 | ✅ | ❌ | Web: `chat/src/lib/notify-settings.ts`; in flight for Android |
| Bookmarks | 0402 | ✅ | ❌ | Android channel list comes from Waddle `discover_topology`, not 0402 |
| Inbox / unread overview | 0430 | ✅ | 🟡 | Android computes unread locally (`UnreadStore.kt`) from live traffic; no 0430 sync |
| Slash commands | — | ✅ | ❌ | Web: `chat/src/lib/slash-dispatch.ts` |
| Room create / configure | 0045 | ✅ | ❌ | Android FFI has only `join_room`/`leave_room`; web has create + edit dialogs |

## Presence & profile

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Presence & status | RFC 6121 | ✅ | ✅ | Android: `PresenceStore.kt` |
| Avatars | 0084, 0153 | ✅ | 🟡 | Android displays avatars only; no publish/upload FFI |
| vCard / profile editing | 0292 | ✅ | ❌ | Web: `chat/src/components/chat/VCardEditor.vue`; Android is display-only |
| Rich presence (mood/activity/tune) | 0107, 0108, 0118 | ✅ | ❌ | Web publishes via pubsub (`client-pubsub.ts`) |
| Idle time | 0319 | ✅ | 🟡 | FFI exposes `idle_since`; not surfaced in Android UI |

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
| Rich text editor / markup | 0394 | ✅ | 🟡 | FFI carries `markup_spans`; Android renders plain body, plain-text composer |
| Markdown rendering | — | ✅ | ❌ | Web: `chat/src/lib/rich-message/markdown.ts` |
| Link previews | urn:waddle:link-preview:0 | ✅ | 🟡 | FFI type present; not rendered in `MessageCard.kt` |
| Stickers | 0449 | ✅ | 🟡 | Android renders received stickers; no picker |
| GIF picker | — | ✅ | ❌ | Web: `chat/src/components/chat/GifPicker.vue` |

## Calls

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| Voice/video calls | 0166, 0353, 0272 | ✅ | 🟡 | DM audio + video calls shipped (CallStore reducer port, LiveKit media, CallStyle notifications, in-call UI, timeline call rows); Muji group calls (0272) pending |
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
| Message moderation (remove others' messages) | 0425 | ✅ | ✅ | Android: `send_moderation` FFI verb wired to action sheet |
| Affiliation management (kick/ban/roles) | 0045 | ✅ | 🟡 | Android has read-only affiliation models; no management actions or UI |
| Hats (role badges) | 0317 | ✅ | 🟡 | FFI exposes `WaddlePresenceHat`; not shown in Android UI |
| Ad-hoc commands | 0050 | ✅ | ❌ | Web: extension-commands infrastructure |
| Server admin pages | — | ✅ | ❌ | Web: `chat/src/pages/admin/[panel].astro` |

## Platform

| Feature | XEP(s) | Web | Android | Notes |
| --- | --- | --- | --- | --- |
| OAuth login (device-auth flow) | — | ✅ | ✅ | Android: full device-authorization flow via Custom Tabs (`WaddleAuthApi.kt`) |
| Stream management + resumption | 0198 | ✅ | ✅ | Android persists an SM resume snapshot across process death |
| Reconnect / connection lifecycle | — | ✅ | ✅ | Android: foreground service (`WaddleConnectionService.kt`) with Kotlin-driven backoff |
| Theme (light/dark/system) | — | ✅ | ✅ | Both clients |

## Planned next

In flight in the current improvement sweep:

1. **Message search** (MAM full-text) — query FFI verb + search UI.
2. **@-mentions** (0372) — composer autocomplete + highlight rendering; FFI payloads already flow.
3. **Per-conversation notify modes / mute** (0492) — settings surface + notification filtering.

Biggest remaining gaps after that, in priority order:

1. **Muji group calls (XEP-0272)** — DM calls shipped; the group-call presence flow, mixer session-initiate, and in-call roster remain.
2. **Room lifecycle + moderation UI** — room create/config, affiliation management (kick/ban), hats display, ad-hoc commands.
3. **Rich content rendering** — markup spans (0394), markdown, link previews, sticker/GIF pickers; most of the data already arrives typed over FFI.
4. **Profile editing** — vCard (0292) and avatar publish (0084); currently display-only.
5. **Group DMs and bookmarks** (0402) — multi-party DMs and server-synced room list.
6. **Community surfaces** — feed (0472), stories (0501), events, extensions; lowest urgency, largest scope.
