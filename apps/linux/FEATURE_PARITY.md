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
| Reactions (quick set via menu) | 0444 | ✅ | #2721: quick reactions in the menu and toolbar, full emoji chooser, replace-set toggle |
| Corrections | 0308 | ✅ | |
| Retraction | 0424 | ✅ | |
| Threads and sub-threads | 0201 | ✅ | |
| Jev judgments (safety scores) | 0422 | ✅ | |
| Delivery states with retry and discard, persisted unsent queue | 0198 | ❌ | delivery |
| Typing indicators (send and show) | 0085 | ❌ | delivery |
| Read markers and cross-device cursors | 0333, 0490 | 🟡 | delivery |
| Shared reconnect controller | | 🟡 | delivery |
| Message search (per conversation) | 0313 | 🟡 | #2721: header search (Ctrl+F), result jumps and pages history in. Capped at 50 results, newest first, no paging; corrections hidden |
| Pinned messages, pin list | urn:waddle:pin | 🟡 | #2721: pin/unpin, header list, jump, live events, row mark. Room pin default is admins-only when unpublished; in a DM the server pin id can differ from the archive id, so the mark may not match |
| Message actions menu parity (copy text, pin, view pack, moderator removal, hover toolbar, keyboard access) | | 🟡 | #2721: one action list for menu and toolbar, pin, copy, moderator removal, focusable rows with Menu/Shift+F10. No view pack |
| `/me` actions | 0245 | ✅ | #2721: italic "* Name action" keeping links; quotes and notifications match |
| Formatting (bold, italic, strike, code, quote) | 0394 | 🟡 | Formatting bar toggle (bold, italic, strikethrough, code, code block, quote; Ctrl+B and Ctrl+I) inserts markdown markers that become XEP-0394 spans on send, as on web and Apple; a correction shows the row's markup as markers and re-sends it. Received spans draw in rows (links only for web addresses; text always escaped). Spans ride in an app-side index beside the timeline, so a message that fell out of it draws plain. Not run against a live server; the bar edits replace the text, so a format step is not its own undo step |

## Conversations and rooms

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Bookmarks and autojoin | 0402 | 🟡 | A room you create is bookmarked with autojoin on, and leaving publishes the bookmark with autojoin off, so the room leaves every device's list; rooms the bookmark says were left are no longer listed. There is no way to browse and rejoin a public room you left, and a rename does not update the bookmark's name (`ui/rooms.rs`, `session_rooms.rs`) |
| Room create, edit, leave, destroy | 0045 | 🟡 | The `+` button above the sidebar opens New Room (name, description, members only, listed, forum); the address comes from the name and is refused when it matches a listed room. Room Details (header button) lets owners edit those settings (GET-merge-SET, only what changed is sent). Leave and, for owners, Delete ask first, and a room its owner deletes elsewhere leaves the list. The pin policy is not editable. Not run against a live server |
| Members, affiliations, kick, ban, hats, bots | 0045, 0317 | 🟡 | Room Details lists the affiliation tiers merged with who is present (role, here or away, hats as badges) and the extension bots under Bots. A person's menu offers what `rooms.rs` allows the account (owner: any affiliation; admin: member or none for people below admin; moderators: kick); kick and ban ask first, and a banned person's menu has Unban. A tier the server refuses is left out, not an error. A person without a real JID (anonymous room) can only be kicked. No profile sheet; hats are display only. Not run against a live server |
| Group DMs: create, rename, add, leave | 0045 | 🟡 | `+` button, New Group Chat (optional name, people found by directory search); listed after the rooms with a people icon and chatted in like a room. Details: rename (empty clears), add people (found by search, not already in, with or without earlier messages), the people list, and leave after confirming. A refused invite fails later and is not shown (as on Apple, #2456). An unnamed group is created named after its members, since the verb takes a name. Not run against a live server |
| Room invites | 0045 | ❌ | rooms (needs core verb, tracked with Apple #2456): the FFI sends only the group-DM mediated invite; it has no verb to invite to a regular room or to answer an invite |
| Forum rooms | | 🟡 | Created and edited with the Forum switch (`muc#roomconfig_forum`) and shown with a forum icon. Topic posts (`forum_post_kind`, `forum_title`) are parsed by the core but not exposed to the Linux UI, and there is no post-title composer (as on Apple, #2456) |
| Inbox / unread overview (`/unread`) | 0430 | 🟡 | Inbox in the sidebar with a total badge: one card per room or DM, newest first, with unread messages and unread thread replies (room threads read with the Waddle MAM thread filter, sub-threads as their own rows), @ mention badges, Mark Read per card and Mark All Read, live updates. Opening a card or message jumps to it. Not run against a live server; DM threads are filtered locally, and mentions in unread history are only known for rooms |
| Notify modes and mute | 0492 | 🟡 | Header bell menu sets all, mentions only or mute per room and DM, stored on the account; muted rows show an icon and notify nothing. No sidebar context menu; a mode changed on another device shows here at the next reconnect |
| Desktop notifications with reply and mark-read actions | | 🟡 | One notification per conversation with Reply (opens it, composer focused; GNotification has no inline reply) and Mark as Read; withdrawn when read here or elsewhere. Not run against a live server or notification daemon |
| @-mentions, slash commands, extension commands | 0372, 0050 | 🟡 | The composer is a multi-line text view: Enter sends, Shift+Enter breaks the line, Tab or Enter picks a suggestion, Escape closes it or cancels a reply or edit. `@` suggests occupants, XEP-0055 directory users with no presence and `@everyone`/`@here`, and typed `@label`s become XEP-0372 references (also on a correction). Extension commands are discovered over XEP-0050 when the session connects and listed with the built-ins; `/word text` fills the command's inline field and completes a single-stage form, anything else opens its XEP-0004 form in a dialog (text, list, boolean and multi-line fields, Back, Next, Cancel; secret fields are never filled). Not run against a live server. A CJK input method's Enter may send while composing (the key is handled before the input method) |
| Nick colors | 0392 | ✅ | Author names in the shared Rust hue at HSL 55%/45%. A room participant is colored by nick, a direct message by the person's address. Initials avatars keep the four brand tints |

## Presence and profile

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Presence picker, status message, auto-away | 0319 | ✅ | Automatic, Online, Away or Do not disturb with a status message, from the account row (also `/active`, `/away`, `/dnd`). A pick is stored on the account (`urn:waddle:status-preference:0`) and taken up by every device, live and on connect. While Automatic: Away after 10 minutes without input, Extended Away after 30, stamped with when input stopped, driven by input in the Waddle window only (not the system idle time). No invisible: the core has none. An open dialog does not follow a pick made on another device |
| Avatars: fetch, publish | 0084, 0153 | 🟡 | Fetched for DM rows, message authors whose address is known, People, profiles and the account row, with the initials fallback; revalidated on `+notify` and after a reconnect, held in memory for the session. Publish (resized to at most 512 px, PNG) and remove from the profile. A room participant whose address the room hides shows initials. XEP-0153 presence hashes are not read, and there is no member list yet (rooms) |
| vCard / profile editor | 0292 | ✅ | Edit your name, nickname, pronouns, about and link; the picture address is kept on save, and the editor stays closed if the current vCard cannot be read. Someone else's opens from their avatar or name, and from People. Only an `http` or `https` link can be opened |
| People view and profile sheet | 6121, 0055 | ✅ | Contacts (by saved name), direct-message peers, anyone whose presence was seen and room participants with an exposed address, split into Here now and Away and offline, with name search that also looks in the community directory. Rows open the profile sheet, which can start a direct message. No call badge until calls land |
| Mood, activity, tune | 0107, 0108, 0118 | 🟡 | Set and clear your own from the profile. The picker leaves out `talking`, and a call mark published by another device is never overwritten or cleared. A tune is typed in. Others' show in their profile sheet, looked up when it opens; there is no live update or list marker |
| Blocking, DND schedule | 0191 | 🚫 | no core verb yet (also open on Apple, #2462, #2463) |

## Media

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Upload, encrypted attachments, drag and drop, paste | 0363, 0448 | ✅ | |
| Inline images, animated GIFs | | ✅ | |
| GIF picker | | 🟡 | |
| Stickers: send, packs | 0449 | 🟡 | |
| Link preview cards | urn:waddle:link-preview:0, 0511 | ✅ | |

The composer's `+` button opens one menu: Photo or File, GIF, Sticker. GIF search goes through the web app's `/api/giphy` proxy (the GIPHY key stays server-side) and sends the picked GIF's address as the body, as web and Apple do. The proxy is the web app's, not the XMPP server's, so its origin is `https://waddle.chat` unless `WADDLE_GIF_PROXY_URL` names another. The sticker page lists the account's own packs (`fetch_sticker_packs`) and sends a sticker with its pack, its picture as an inline file and its description as the text. A received sticker draws without its text; clicking it opens its pack, on the sender's own sticker node only, with "Add to My Stickers" (`import_sticker_pack`) unless the author marked it restricted. Server-attached link previews render as cards under the message (site, title, description, the server-cached image); a click opens `http` or `https` targets in the browser.

Gaps: no sticker pack creation or removal (the FFI has `publish_sticker_pack` and `retract_sticker_pack`; there is no editor yet); sticker pictures are decoded as a still frame, so animated stickers do not play; a sticker in a room opens its pack only from archive rows (live room messages do not expose the sender's real JID); the picker has no pack tabs; no video or player-embed cards; no composer-side link preview lookup (`lookup_link_preview` is not called, so this client's own messages carry no preview token, as on Apple); `fetch_external_services` is not used. Previews and sticker markers ride in an app-side index beside the shared timeline (`waddle-client-session` has no place for them), keyed by conversation and message id and bounded, so a card on a message that has fallen out of it is not drawn. Not tested against a live server.

## Community

| Feature | XEP(s) | Now | Closed by |
| --- | --- | --- | --- |
| Feed | 0472 | 🟡 | Feed in the sidebar: the community feed (`community.<domain>`), newest first, with avatars and XEP-0392 nick colors, names from profiles, links in posts opening in the browser (http and https only, no credentials in the address), server-written posts (mood, activity, profile) labelled. Compose a post with text and a link (https assumed when none is typed), or a picture: the feed entry has no media field, so a picture is uploaded as it is (not encrypted, the community reads it) and travels as the post's link, shown inline. No live updates: refresh button, and the feed is fetched again when opened after a minute. First 50 posts only, no paging. Not run against a live server |
| Stories | 0501, 0470 | 🟡 | A rail along the top of the Feed: your own ring first, then people with unseen stories, newest first. The viewer steps through a person's stories and on through the next ones (buttons, Left and Right, or the sides of the picture), advances after 6 seconds, pauses with Space or the middle of the picture, and reacts with emoji (six quick ones or the emoji picker, up to 12; writes are sent one at a time per story so quick taps end as the last one left them). Seen stories are merged with and published to the account's private read state, only when this device adds to it. Share a story from Add Story: a picture (PNG, JPEG, GIF or WebP, up to 10 MB) with a caption, up for a day. Video stories are not played: the viewer offers to open the video in the browser. No live updates and no list of who reacted. Not run against a live server |
| Events and calendar, RSVP | | ❌ | events |
| Admin console (owners) | | ❌ | admin |
| 1:1 and group voice/video, screen share | 0353, 0166, Muji | ❌ | calls |

## Platform

| Feature | Now | Closed by |
| --- | --- | --- |
| Device-flow sign-in, Secret Service storage, multi-account | ✅ | |
| Settings screen (appearance, message order, notifications, accounts) | ❌ | settings |
| Polar Huddle design tokens, Night and Daylight | 🟡 | polish |
| Keyboard shortcuts, accessibility pass | 🟡 | Composer keys (Enter, Shift+Enter, Tab, Escape, Ctrl+B, Ctrl+I) landed with the composer; the rest is polish |
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
10. ~~feed and stories~~ (done), **events**, **admin**.
11. **delivery**: ack states, queue, typing, shared reconnect.
12. **calls**.
13. **polish**: design tokens, shortcuts, accessibility, Flatpak.
