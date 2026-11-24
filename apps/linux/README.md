# Waddle for Linux

A native GTK4 + libadwaita client, in Rust. The plan is `claude/linux-client-plan.md` (in the Waddle Project) and ADR-0019 ("One client state layer in Rust").

## Status

The native client currently supports:

- **Sign-in:** RFC 8628 device flow against any Waddle server, the same flow the Apple and Android apps use. Pick a provider, approve in the browser, and the window continues by itself.
- **Accounts:** stored in the Secret Service (`oo7`), one item per server and JID, so several communities can be signed in at once.
- **Shell:** an adaptive `AdwNavigationSplitView` that collapses at phone widths.
- **Chat:** rooms and DMs, paged history, replies, reactions, edits, deletions and attachments.
- **Media:** the composer's `+` menu offers Photo or File, a GIF picker (search and trending through the web app's `/api/giphy` proxy; set `WADDLE_GIF_PROXY_URL` to use a web app other than `https://waddle.chat`) and your sticker packs (XEP-0449). Received stickers draw in place, and the link preview cards the server attaches show under messages.
- **Inbox:** the sidebar's **Inbox** lists every room or DM with unread messages, newest first, with unread thread replies under each card. Click a card or message to open it there; **Mark Read** clears one card and **Mark All Read** clears everything.
- **Calendar:** the sidebar's **Calendar** lists community events (xCal) as an agenda, grouped by day on your clock, or a month with marked days. An event shows its time, description, location and who is going; answer Going, Maybe or Not Going. The community owner can create, edit, delete and cancel single dates of repeating events, and open or create the event's room. Dates and recurrences are expanded by the shared core (`src/events.rs`).
- **Threads:** choose **Reply in Thread** below a message, or **Open Thread** from its message menu. Threads appear beneath their conversation in the sidebar, with separate unread counts and history. **Start Subthread** creates a nested thread; the back button returns to the main conversation. The original message appears above the replies when loaded. Thread views filter full archive pages so saved reactions, edits and Jev judgments remain available. Automatic scanning stops after three pages; **Load Earlier** continues through older history.
- **Profile and presence:** the account row opens your profile: picture, status (Automatic, Online, Away, Do not disturb) with a message, name, nickname, pronouns, about and link, and your mood, activity and tune. A picked status is stored on the account and taken up by every device; Automatic goes Away after 10 minutes without input in the window and Extended Away after 30. Avatars show in the sidebar, the timeline and People, and author names use their XEP-0392 color. Click an avatar or name for a profile; **People** in the sidebar lists everyone the account knows.
- **Jev badges:** question and content-safety judgments appear beneath messages. Expand a badge to inspect probabilities, model and taxonomy versions. Judgments follow the exact message revision and disappear after an edit or deletion until a current result arrives.
- **Message actions:** one list (`src/message_actions.rs`) decides what a message offers: add reaction (quick reactions and the full emoji chooser), reply, open thread, pin or unpin, copy text, edit and delete your own, and moderator removal in rooms. A toolbar floats over the row on hover or keyboard focus; right-click, long press, the Menu key or Shift+F10 opens the full menu.
- **Pins:** the pin button in the conversation header lists pinned messages (`urn:waddle:pin:0`) and jumps to one; pin events update the list live and pinned rows are marked. A room that does not publish a pin policy is treated as admins-only, the server default.
- **Search:** the search button (Ctrl+F) searches the open conversation's archive (XEP-0313); a result jumps to the message, paging older history in until it is loaded.
- **`/me` actions (XEP-0245):** shown as italic "* Name action"; quotes and notifications show the same text.
- **Notifications:** the bell in the conversation header sets all messages, mentions only or mute for that room or DM (XEP-0492, stored on the account and shared with its other devices); muted conversations show a bell-slash in the sidebar. Without a stored mode, DMs and members-only rooms notify for every message and other rooms for mentions. Desktop notifications are one per conversation (a newer message replaces the older), have **Reply** (opens the conversation with the composer focused; GNotification has no inline reply) and **Mark as Read** actions, and are withdrawn when the conversation is read here or on another device.

- **Rooms and group chats:** the `+` button above the sidebar creates a room (name, description, members only, listed, forum) or a group chat (people found in the user directory). **Room Details** in the conversation header edits an owner's room settings, lists its people with affiliation, role and hats and its bots, offers promote, demote, kick and ban to those allowed (kick and ban ask first), and leaves or, for owners, deletes the room after confirmation. A group chat's details rename it, add people and leave it. Not yet: browsing and rejoining a room you left, inviting to a regular room, forum topic posts.

The timeline and judgment lifecycle use `waddle-client-session` (ADR-0019).

## Build

Use the `linux` dev shell, which has GTK 4, libadwaita, pkg-config and blueprint-compiler:

```sh
nix develop .#linux
cd apps/linux
cargo run
```

The baseline is GNOME 48 (GTK 4.18, libadwaita 1.7).

## Checks

CI runs these through `apps/linux/env.cue`:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
desktop-file-validate data/social.waddle.Waddle.desktop
appstreamcli validate --no-net data/social.waddle.Waddle.metainfo.xml
```

The GTK window tests require a display and are excluded from ordinary headless runs. GTK can start once per process, so run each separately: `xvfb-run -a cargo test --locked thread_navigation_and_context -- --ignored --test-threads=1`, then the same with `profile_surfaces_open_and_resolve_authors` (the profile, People and profile-sheet dialogs, built against a session that cannot connect).

## Layout

| Path | What |
|---|---|
| `src/auth/device.rs` | Device-flow client; typed server URL, session id and JID; tests against a local stand-in server |
| `src/auth/store.rs` | Secret Service account store |
| `src/runtime.rs` | tokio on its own threads; UI awaits work from the GLib main loop |
| `src/message_actions.rs`, `src/pins.rs`, `src/jump.rs`, `src/message_text.rs` | Message action rules, the pinned list, jump planning and body rendering, without widgets |
| `src/session_tools.rs`, `src/ui/message_tools.rs` | Pin, search and moderation verbs; header buttons, search bar, hover toolbar |
| `src/rooms.rs`, `src/session_rooms.rs`, `src/ui/rooms.rs` | Room rules (addresses, setting changes, who may moderate whom, member list, group DM members), the room and group DM verbs, and the dialogs |
| `src/composer.rs` | `@` mention and slash command suggestions and actions (`/me`, `/shrug`, `/away`, `/active`, `/dnd`) |
| `src/presence.rs`, `src/avatars.rs`, `src/profile.rs`, `src/people.rs`, `src/nick_color.rs` | Presence mode and auto-away, avatar cache, profile fields and registries, the People listing, nick colors: all without widgets |
| `src/ui/status.rs`, `pictures.rs`, `profile.rs`, `person.rs`, `people.rs`, `identity.rs` | Their windows: presence plumbing, avatar loading and publishing, your profile, someone's profile sheet, the People view, author resolution |
| `src/ui/window.rs`, `data/ui/window.blp` | Window and sign-in steps (Blueprint template) |
| `src/notify.rs` | Notification modes, XEP-0492 defaults and the announce decision (no GTK); tests |
| `src/ui/notifications.rs` | Header menu, sidebar mute state and GNotification actions |
| `src/ui/suggestions.rs` | The composer's suggestion popover |
| `data/` | Desktop entry, AppStream metadata, icon |
