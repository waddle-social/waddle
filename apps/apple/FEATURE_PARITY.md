# Feature parity: Waddle Apple apps (iOS + macOS)

## Context

The Apple apps (`apps/apple/`, SwiftUI) are a thin UI over the same Rust XMPP
core the web and Android clients use
(`server/crates/waddle-xmpp-client-ffi`, UniFFI bindings generated into
`Waddle/RustClient/Generated/`). Every protocol verb listed below already
exists in the FFI; a gap on Apple is a Swift gap, not a protocol gap.

This table is the audit baseline taken on 2026-09-22 before the Apple
rebuild. The "Baseline" column records what `main` shipped at that point;
the rebuild PR updates the "Now" column as features land.

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
| Channel (MUC) messaging | 0045 | 🟡 | | Baseline reconciled own echoes by body text, so two identical sends collided |
| 1:1 direct messages | — | 🟡 | | Baseline appended live and archived copies without dedupe |
| Group DMs | 0045 | ❌ | | |
| History (MAM) | 0313 | 🟡 | | Rooms paged; DMs fetched one page only |
| Message search | 0313 | ❌ | | |
| Corrections | 0308 | 🟡 | | Rendered, never sent |
| Retraction | 0424 | ❌ | | `retractMessage` was an empty stub |
| Reactions | 0444 | 🟡 | | Rendered, never sent |
| Replies | 0461, 0428 | ✅ | | |
| Threads | 0201 | 🟡 | | |
| Delivery acks | 0198 | ❌ | | Acks were logged and dropped |
| Typing / chat states | 0085 | 🟡 | | Received; `sendChatState` was an empty stub |
| Read markers + sync | 0333, 0490 | ❌ | | `sendDisplayedMarker` was an empty stub |

## Conversation features

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Pinned messages | urn:waddle:pin:0 | ❌ | | |
| @-mentions | 0372 | 🟡 | | Regex highlight only; no references sent |
| Nick colors | 0392 | ❌ | | |
| Notify modes / mute | 0492 | ❌ | | |
| Inbox / unread counts | 0430 | ❌ | | Unread counts were hardcoded to 0 |
| Room create | 0045 | ❌ | | `createChannel` was a stub returning nil |
| Slash commands | 0050 | ❌ | | |

## Presence & profile

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Presence | RFC 6121 | 🟡 | | |
| Avatars | 0084 | 🟡 | | Fetch only |
| vCard / profile | 0292 | ❌ | | |
| Mood / activity / tune | 0107, 0108, 0118 | ❌ | | All publish verbs were empty stubs |

## Notifications

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Local notifications | — | ❌ | | In-app toast for broadcast mentions only |
| Push (APNs) | 0357 + push.<domain> 0050 | ❌ | | `enablePushNotifications` was an empty stub |

## Media

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| File upload + attachments | 0363, 0447 | ✅ | | |
| Encrypted attachments | 0448 | ❌ | | |
| Link previews | urn:waddle:link-preview:0 | ❌ | | |
| Stickers | 0449 | 🟡 | | Render only |
| GIF picker | — | ❌ | | Baseline shipped six hardcoded GIF URLs |

## Calls, community, admin

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Voice/video calls | 0166, 0353, 0272 | ❌ | | Call events logged and dropped |
| Moderation | 0425 | ❌ | | |
| Affiliations / roles | 0045 | ❌ | | Baseline called `/v1/space/members`, which the server does not serve |
| Hats | 0317 | 🟡 | | |

## Platform

| Feature | XEP(s) | Baseline | Now | Notes |
| --- | --- | --- | --- | --- |
| Device-auth sign-in | — | ✅ | | |
| Session credential storage | — | ❌ | | Session id stored in `UserDefaults`, not the Keychain |
| Reconnect | — | 🟡 | | Fixed 1.5 s retry, no backoff, no network awareness |
| XMPP-native data plane | — | ❌ | | Baseline called `/v1/space`, `/v1/channels`, `/v1/space/members`, `/api/users/search` over HTTP; none exist on the server |
| Automated tests | — | ❌ | | No test target |
