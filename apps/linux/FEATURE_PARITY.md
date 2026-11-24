# Feature parity: Waddle for Linux

The Linux app (`apps/linux/`, GTK4 + libadwaita) is a thin UI over the same
Rust core the web, Android and Apple clients use
(`server/crates/waddle-xmpp-client-ffi`, linked directly as Rust, and the shared
timeline store in `server/crates/waddle-client-session`). Unless a row says
otherwise, the protocol verb already exists in the FFI, so a gap here is a UI
gap, not a protocol gap. The reference implementations are the web client
(`chat/`) and the Apple apps (`apps/apple/FEATURE_PARITY.md`).

Linux has no users, so rows are closed by replacing, not by shimming.

| Symbol | Meaning |
| --- | --- |
| ✅ | Works end to end |
| 🟡 | Partial |
| ❌ | Missing |
| 🚫 | Out of scope on Linux (reason in Notes) |

Each row names the PR that closes it. The audit date is 2026-11-24, against
`main` at 275a624.

## Core messaging

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Rooms and 1:1 DMs, paged history | 0045, 0313 | ✅ | |
| Replies | 0461 | ✅ | |
| Reactions (quick set via menu) | 0444 | 🟡 | message actions |
| Corrections | 0308 | ✅ | |
| Retraction | 0424 | ✅ | |
| Threads and sub-threads | 0201 | ✅ | |
| Jev judgments (safety scores) | 0422 | ✅ | |
| Delivery states with retry and discard, persisted unsent queue | 0198 | ❌ | delivery |
| Typing indicators (send and show) | 0085 | ❌ | delivery |
| Read markers and cross-device cursors | 0333, 0490 | 🟡 | delivery |
| Shared reconnect controller | | 🟡 | delivery |
| Message search (per conversation) | 0313 | ❌ | message actions |
| Pinned messages, pin list | urn:waddle:pin | ❌ | message actions |
| Message actions menu parity (copy text, pin, view pack, moderator removal, hover toolbar, keyboard access) | | 🟡 | message actions |
| `/me` actions | 0245 | ❌ | message actions |
| Formatting (bold, italic, strike, code, quote) | 0394 | ❌ | composer |

## Conversations and rooms

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Bookmarks and autojoin | 0402 | 🟡 | rooms |
| Room create, edit, leave, destroy | 0045 | ❌ | rooms |
| Members, affiliations, kick, ban, hats, bots | 0045 | ❌ | rooms |
| Group DMs: create, rename, add, leave | 0045 | ❌ | rooms |
| Room invites | 0045 | ❌ | rooms (needs core verb, tracked with Apple #2456) |
| Forum rooms | | ❌ | rooms |
| Inbox / unread overview (`/unread`) | 0430 | 🟡 | inbox |
| Notify modes and mute | 0492 | 🟡 | Header bell menu sets all, mentions only or mute per room and DM, stored on the account; muted rows show an icon and notify nothing. No sidebar context menu; a mode changed on another device shows here at the next reconnect |
| Desktop notifications with reply and mark-read actions | | 🟡 | One notification per conversation with Reply (opens it, composer focused; GNotification has no inline reply) and Mark as Read; withdrawn when read here or elsewhere. Not run against a live server or notification daemon |
| @-mentions, slash commands, extension commands | 0372, 0050 | 🟡 | composer |
| Nick colors | 0392 | ❌ | profile |

## Presence and profile

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Presence picker, status message, auto-away | | 🟡 | profile |
| Avatars: fetch, publish | 0084, 0153 | ❌ | profile |
| vCard / profile editor | 0292 | ❌ | profile |
| People view and profile sheet | | ❌ | profile |
| Mood, activity, tune | 0107, 0108, 0118 | ❌ | profile |
| Blocking, DND schedule | 0191 | 🚫 | no core verb yet (also open on Apple, #2462, #2463) |

## Media

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Upload, encrypted attachments, drag and drop, paste | 0363, 0448 | ✅ | |
| Inline images, animated GIFs | | ✅ | |
| GIF picker | | ❌ | media |
| Stickers: send, packs | | ❌ | media |
| Link preview cards | | ❌ | media |

## Community

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Feed | | ❌ | feed and stories |
| Stories | | ❌ | feed and stories |
| Events and calendar, RSVP | | ❌ | events |
| Admin console (owners) | | ❌ | admin |
| 1:1 and group voice/video, screen share | 0353, 0166, Muji | ❌ | calls |

## Platform

| Feature | Now | Closed by |
| --- | --- | --- |
| Device-flow sign-in, Secret Service storage, multi-account | ✅ | |
| Settings screen (appearance, message order, notifications, accounts) | ❌ | settings |
| Polar Huddle design tokens, Night and Daylight | 🟡 | polish |
| Keyboard shortcuts, accessibility pass | ❌ | polish |
| Push | 🚫 | Linux runs a background session for notifications; no APNs/FCM equivalent |
| Flatpak packaging | ❌ | polish |

## Plan

PRs, in this order. Each is one concern, built on the one before only where it
must be.

1. This document.
2. **notifications**: notify modes and mute, notification actions.
3. **profile**: presence picker, avatars, vCard editor, nick colors, People.
4. **message actions**: pins, search, `/me`, menu parity, hover toolbar.
5. **composer**: 0394 formatting bar, extension commands, XEP-0372 reference parity.
6. **rooms**: create, edit, leave, members and moderation, group DMs, forum.
7. **inbox**: unread overview.
8. **media**: GIF picker, stickers, link previews.
9. **settings**: one preferences window.
10. **feed and stories**, **events**, **admin**.
11. **delivery**: ack states, queue, typing, shared reconnect.
12. **calls**.
13. **polish**: design tokens, shortcuts, accessibility, Flatpak.
