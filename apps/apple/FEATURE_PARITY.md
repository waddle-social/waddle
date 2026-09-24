# Feature parity: Waddle Apple apps (iOS + macOS)

## Context

The Apple apps (`apps/apple/`, SwiftUI) are a thin UI over the same Rust XMPP
core the web and Android clients use
(`server/crates/waddle-xmpp-client-ffi`, UniFFI bindings generated into
`Waddle/RustClient/Generated/`). Every protocol verb listed below already
exists in the FFI; a gap on Apple is a Swift gap, not a protocol gap.

The "Baseline" column records what `main` shipped on 2026-09-22 before the
Apple rebuild; "Now" is the state after the rebuild (PR #1822).

## Legend

| Symbol | Meaning |
| --- | --- |
| ✅ | Works end to end |
| 🟡 | Partial: renders or half-wired, not complete |
| ❌ | Missing, or a stub that silently does nothing |
| 🚫 | Out of scope on Apple (reason in Notes) |

## Core messaging

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Channel (MUC) messaging | 0045 | 🟡 | ✅ | Local echo keyed by client stanza id, superseded in place by the room reflection. Baseline: reconciled own echoes by body text, so two identical sends collided |
| 1:1 direct messages | — | 🟡 | ✅ | XEP-0359 dedupe across live, carbon and archive copies. Baseline: appended live and archived copies without dedupe |
| Group DMs | 0045 | ❌ | 🟡 | Create, list and chat; rename, invite and leave not surfaced yet |
| History (MAM) | 0313 | 🟡 | ✅ | RSM paging for rooms and DMs; refetch of the newest page after reconnect. Baseline: rooms paged; DMs fetched one page only |
| Message search | 0313 | ❌ | ✅ | Per-conversation archive search; a result jumps to the message, paging older history in when it is not loaded |
| Corrections | 0308 | 🟡 | ✅ | Full-stanza re-send per XEP-0308, keeping the original's mentions and markup; received corrections replace markup and references. Baseline: rendered, never sent |
| Retraction | 0424 | ❌ | ✅ | Room retractions wait for the reflection. Baseline: `retractMessage` was an empty stub |
| Reactions | 0444 | 🟡 | ✅ | XEP-0444 replace-set semantics, room-assigned ids only in rooms. Baseline: rendered, never sent |
| Replies | 0461, 0428 | ✅ | ✅ | Reply ids follow XEP-0461 (room-assigned ids in rooms) |
| Threads | 0201 | 🟡 | ✅ | Thread panel (inspector on iPad/Mac), reply counts |
| Delivery acks | 0198 | ❌ | ✅ | Sending / queued / sent / acknowledged / failed with retry and discard; unsent and unconfirmed messages survive the app being killed and resend with the same origin-id. Baseline: acks were logged and dropped |
| Typing / chat states | 0085 | 🟡 | ✅ | Sent with pause timeout; received with expiry. Baseline: received; `sendChatState` was an empty stub |
| Read markers + sync | 0333, 0490 | ❌ | ✅ | XEP-0333 (room ids, 1:1 @id when requested) and XEP-0490 cursors. Baseline: `sendDisplayedMarker` was an empty stub |

## Conversation features

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Pinned messages | urn:waddle:pin:0 | ❌ | ✅ | Pin/unpin, pinned list, live pin events |
| `/me` actions | 0245 | ❌ | ✅ | Rendered as "* Name action" in italics, keeping the action's markup, mentions and links; conversation-list, reply and VoiceOver previews match |
| @-mentions | 0372 | 🟡 | ✅ | Autocomplete sends XEP-0372 references; self-mentions highlighted and alerted. Baseline: regex highlight only; no references sent |
| Nick colors | 0392 | ❌ | ✅ | Shared Rust hue, HSL 55%/45% like web |
| Notify modes / mute | 0492 | ❌ | ✅ | Per conversation, drives local notifications |
| Inbox / unread counts | 0430 | ❌ | ✅ | Server-authoritative with read-clear barrier; mention badges; app badge. Baseline: unread counts were hardcoded to 0 |
| Room create | 0045 | ❌ | ✅ | Baseline: `createChannel` was a stub returning nil |
| Slash commands | 0050, 0004 | ❌ | ✅ | Built-ins (`/me`, `/shrug`, `/giphy`, `/away`, `/active`, `/dnd`) plus server extension commands discovered over XEP-0050 at session start; popover completion, inline single-field execution, XEP-0004 form sheet for multi-step commands |
| Composer | 0394, 0372 | 🟡 | ✅ | Slack-style card: `+` menu (photo, file, GIF), `Aa` formatting bar (bold, italic, strikethrough, code, code block, quote as XEP-0394 markup; links inserted as bare URLs), emoji, `@` and `/` buttons, send |
| Paste images / files / GIFs | 0363, 0447 | ❌ | ✅ | Pasted files, animated GIFs (bytes kept) and still images upload as attachments; text and rich-text selections paste as text |

## Presence & profile

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Presence | RFC 6121 | 🟡 | ✅ | Availability and status message |
| Avatars | 0084 | 🟡 | ✅ | Fetch, publish (≤512 px PNG) and remove. Baseline: fetch only |
| vCard / profile | 0292 | ❌ | ❌ | Not in this PR |
| Mood / activity / tune | 0107, 0108, 0118 | ❌ | 🟡 | Mood only (XEP-0107 vocabulary). Baseline: all publish verbs were empty stubs |

## Notifications

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Local notifications | — | ❌ | ✅ | Inline reply and mark-read actions, grouped per conversation. Baseline: in-app toast for broadcast mentions only |
| Push (APNs) | 0357 + push.<domain> 0050 | ❌ | ✅ | Client registers with push.<domain>; the Push Service delivers a minimal alert (class-specific banner, badge, routing context; no sender or body) and a tap opens the conversation on the right account. Baseline: `enablePushNotifications` was an empty stub |

## Media

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| File upload + attachments | 0363, 0447 | ✅ | ✅ | Photos re-encoded as JPEG without location data |
| Encrypted attachments | 0448 | ❌ | 🟡 | Received files are downloaded, verified (XEP-0300 sha-256/sha-512, GCM tag) and decrypted in memory (AES-128/256-GCM with or without tag, AES-256-CBC), then shown like plain attachments. Sending encrypted files is not implemented |
| Link previews | urn:waddle:link-preview:0 | ❌ | ✅ | Renders server-attached cards |
| Stickers | 0449 | 🟡 | 🟡 | Render only; GIF stickers play |
| Animated GIFs | 0447, 0448 | ❌ | ✅ | Shared GIFs (plain and decrypted XEP-0448) and bodies that are a single `https` image or Giphy URL render inline like web, GIFs playing; first frame only with Reduce Motion. Baseline: first frame only; image URL bodies showed as text |
| GIF picker | — | ❌ | ✅ | GIPHY search through the server's web-app `/api/giphy` proxy (the Giphy key stays server-side), debounced, trending when empty; sends the GIF URL as the body, as web does. Baseline: shipped six hardcoded GIF URLs |

## Calls, community, admin

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Voice/video calls | 0166, 0353, 0272 | ❌ | ❌ | Not in this PR. Baseline: call events logged and dropped |
| Moderation | 0425 | ❌ | ✅ | Moderator-gated with confirmation |
| Affiliations / roles | 0045 | ❌ | ✅ | XEP-0045 member lists, affiliation changes, kick, ban. Baseline: called `/v1/space/members`, which the server does not serve |
| Hats | 0317 | 🟡 | ✅ | Shown as author and member badges |

## Platform

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Device-auth sign-in | — | ✅ | ✅ | RFC 8628 device code with a server switcher |
| Session credential storage | — | ❌ | ✅ | Keychain, per server; random per-install XMPP resource. Baseline: session id stored in `UserDefaults`, not the Keychain |
| Reconnect | — | 🟡 | ✅ | Jittered exponential backoff, 15 s connect budget, resume on foreground. Baseline: fixed 1.5 s retry, no backoff, no network awareness |
| XMPP-native data plane | — | ❌ | ✅ | All HTTP data-plane calls removed. Baseline: called `/v1/space`, `/v1/channels`, `/v1/space/members`, `/api/users/search` over HTTP; none exist on the server |
| Automated tests | — | ❌ | ✅ | WaddleKit Swift Testing suite in CI. Baseline: no test target |
