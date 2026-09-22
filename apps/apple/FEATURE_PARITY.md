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
| Message search | 0313 | ❌ | 🟡 | Per-conversation archive search; results do not jump to the message yet |
| Corrections | 0308 | 🟡 | ✅ | Full-stanza re-send per XEP-0308; received corrections replace markup and references. Baseline: rendered, never sent |
| Retraction | 0424 | ❌ | ✅ | Room retractions wait for the reflection. Baseline: `retractMessage` was an empty stub |
| Reactions | 0444 | 🟡 | ✅ | XEP-0444 replace-set semantics, room-assigned ids only in rooms. Baseline: rendered, never sent |
| Replies | 0461, 0428 | ✅ | ✅ | Reply ids follow XEP-0461 (room-assigned ids in rooms) |
| Threads | 0201 | 🟡 | ✅ | Thread panel (inspector on iPad/Mac), reply counts |
| Delivery acks | 0198 | ❌ | ✅ | Sending / queued / sent / acknowledged / failed with retry and discard. Baseline: acks were logged and dropped |
| Typing / chat states | 0085 | 🟡 | ✅ | Sent with pause timeout; received with expiry. Baseline: received; `sendChatState` was an empty stub |
| Read markers + sync | 0333, 0490 | ❌ | ✅ | XEP-0333 (room ids, 1:1 @id when requested) and XEP-0490 cursors. Baseline: `sendDisplayedMarker` was an empty stub |

## Conversation features

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Pinned messages | urn:waddle:pin:0 | ❌ | ✅ | Pin/unpin, pinned list, live pin events |
| @-mentions | 0372 | 🟡 | ✅ | Autocomplete sends XEP-0372 references; self-mentions highlighted and alerted. Baseline: regex highlight only; no references sent |
| Nick colors | 0392 | ❌ | ✅ | Shared Rust hue, HSL 55%/45% like web |
| Notify modes / mute | 0492 | ❌ | ✅ | Per conversation, drives local notifications |
| Inbox / unread counts | 0430 | ❌ | ✅ | Server-authoritative with read-clear barrier; mention badges; app badge. Baseline: unread counts were hardcoded to 0 |
| Room create | 0045 | ❌ | ✅ | Baseline: `createChannel` was a stub returning nil |
| Slash commands | 0050 | ❌ | ❌ | Not in this PR |

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
| Push (APNs) | 0357 + push.<domain> 0050 | ❌ | 🟡 | Client registers with push.<domain>; server APNs dispatch is still stubbed (#529). Baseline: `enablePushNotifications` was an empty stub |

## Media

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| File upload + attachments | 0363, 0447 | ✅ | ✅ | Photos re-encoded as JPEG without location data |
| Encrypted attachments | 0448 | ❌ | ❌ | Shown as a locked card; no decryption yet |
| Link previews | urn:waddle:link-preview:0 | ❌ | ✅ | Renders server-attached cards |
| Stickers | 0449 | 🟡 | 🟡 | Render only |
| GIF picker | — | ❌ | ❌ | Removed the fake picker; not rebuilt. Baseline: shipped six hardcoded GIF URLs |

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
