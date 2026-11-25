# Waddle for Linux

A native GTK4 + libadwaita client, in Rust. The plan is `claude/linux-client-plan.md` (in the Waddle Project) and ADR-0019 ("One client state layer in Rust").

## Status

The native client currently supports:

- **Sign-in:** RFC 8628 device flow against any Waddle server, the same flow the Apple and Android apps use. Pick a provider, approve in the browser, and the window continues by itself.
- **Accounts:** stored in the Secret Service (`oo7`), one item per server and JID, so several communities can be signed in at once.
- **Shell:** an adaptive `AdwNavigationSplitView` that collapses at phone widths.
- **Chat:** rooms and DMs, paged history, replies, reactions, edits, deletions and attachments.
- **Composer:** a multi-line text view; Enter sends, Shift+Enter breaks the line. The formatting button opens a bar (bold, italic, strikethrough, code, code block, quote; Ctrl+B and Ctrl+I) that inserts markdown markers, which become XEP-0394 markup on send and draw in received messages. Slash suggestions list the built-ins and the server's XEP-0050 extension commands, which run inline or open their XEP-0004 form in a dialog.
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
- **Preferences:** Ctrl+comma or the main menu. Appearance (follow the system, light or dark), message order (Chat, newest at the bottom; Social, newest first with the composer on top), notifications (on or off, and whether rooms with no mode of their own notify only for mentions; DMs always notify) and accounts (sign out, switch, add). They are kept in `$XDG_CONFIG_HOME/waddle/settings.json`; sessions stay in the Secret Service.
- **Notifications:** the bell in the conversation header sets all messages, mentions only or mute for that room or DM (XEP-0492, stored on the account and shared with its other devices); muted conversations show a bell-slash in the sidebar. Without a stored mode, DMs and members-only rooms notify for every message and other rooms for mentions. Desktop notifications are one per conversation (a newer message replaces the older), have **Reply** (opens the conversation with the composer focused; GNotification has no inline reply) and **Mark as Read** actions, and are withdrawn when the conversation is read here or on another device.
- **Admin:** community owners get an **Admin** row in the sidebar (shown once the server accepts the owner probe; the server re-checks every command). **Spaces** lists spaces to create, edit and delete; opening one lists its channels (create, edit, delete) and members (change role, or remove). Opening a channel lists its affiliations (change standing, add by address, ban) and who is in the room now (remove with an optional reason). **Users** lists the community's accounts with a prefix filter. Deleting, removing, banning and lowering your own role all ask first. The server sets the owner hat and has no verb to change it, so the console shows it but cannot grant it.

- **Rooms and group chats:** the `+` button above the sidebar creates a room (name, description, members only, listed, forum) or a group chat (people found in the user directory). **Room Details** in the conversation header edits an owner's room settings, lists its people with affiliation, role and hats and its bots, offers promote, demote, kick and ban to those allowed (kick and ban ask first), and leaves or, for owners, deletes the room after confirmation. A group chat's details rename it, add people and leave it. Not yet: browsing and rejoining a room you left, inviting to a regular room, forum topic posts.

- **Delivery, typing and read state:** your messages show Waiting to send, Sending…, Not sent (with **Retry** and **Discard**) and Sent or Read on the newest. Text messages are kept in `$XDG_STATE_HOME/waddle/` until the server acknowledges them and are sent again, with the same id, after a dropped connection or a restart. "Name is typing…" shows under the timeline and your own typing is sent. Reading a conversation tells the other side (XEP-0333) and your other devices (XEP-0490). A banner shows when the connection is lost or the network is gone; the client reconnects with backoff, and at once when the window is focused or the network returns.

- **Voice calls:** the phone button in a one-to-one conversation's header places an audio call (XEP-0353 invitations, then a XEP-0166 session with LiveKit credentials from the server, as on the Apple and web apps). An incoming call opens the call window with Answer and Decline, and posts a notification with the same actions when the app is not in front. The window shows the state, the elapsed time, a mute toggle, microphone and speaker choice, and Hang Up. Audio only for now: no video, screen share or group calls, and no ring tone. Not yet run against a live server or audio hardware.

- **Keyboard and accessibility:** Ctrl+? (or Keyboard Shortcuts in the main menu) lists every shortcut; Ctrl+K switches conversation by name, Alt+Down and Alt+Up step through the sidebar, Ctrl+L focuses the composer. Icon-only controls, unread counts, message rows and story rings have accessible names; see `FEATURE_PARITY.md` for what was and was not checked. Colours are the Polar Huddle tokens (`data/style.css`), Night or Daylight following the Preferences appearance setting.

The timeline and judgment lifecycle use `waddle-client-session` (ADR-0019).

## Build

Use the `linux` dev shell, which has GTK 4, libadwaita, pkg-config and blueprint-compiler:

```sh
nix develop .#linux
cd apps/linux
cargo run
```

The baseline is GNOME 48 (GTK 4.18, libadwaita 1.7).

Call media links libwebrtc through the `livekit` crate. Its build script downloads a prebuilt libwebrtc archive from the `livekit/rust-sdks` GitHub releases on the first build (about 150 MB, 700 MB unpacked, kept under `target/`; set `LK_CUSTOM_WEBRTC` to use a local build), and compiles its C++ bridges against Chromium's libc++, which needs clang 21 or later as the C++ compiler. The `linux` dev shell provides it and sets `CXX`; outside it, install clang 21+ and export `CXX=clang++`. At run time libwebrtc opens PulseAudio (or PipeWire's pulse server) and ALSA.

## Flatpak

`flatpak/social.waddle.Waddle.yaml` builds against the GNOME 50 runtime with the Rust and LLVM SDK extensions. It was written without being run (no `flatpak-builder`, no network where it was authored), so treat it as a starting point:

```sh
cd apps/linux/flatpak
# crate sources for the offline build, from Cargo.lock (flatpak-builder-tools)
python3 flatpak-cargo-generator.py ../Cargo.lock -o cargo-sources.json
flatpak-builder --user --install --force-clean build-dir social.waddle.Waddle.yaml
```

Permissions: Wayland and X11 fallback, IPC, DRI, PulseAudio (the microphone and speakers for calls; PipeWire answers the protocol), and the network. There is no filesystem access (the file chooser and drop targets use the document portal), no notification talk-name (GNotification uses the portal) and no Secret Service talk-name (oo7 uses the secret portal).

libwebrtc: the `livekit` crate's build script downloads a prebuilt libwebrtc archive from GitHub, which the build sandbox forbids. The manifest therefore lists that archive (tag `webrtc-a545de4`, the `WEBRTC_TAG` of the locked `webrtc-sys-build`, one zip per architecture, with sha256) as a source and points `LK_CUSTOM_WEBRTC` at the unpacked copy. The checksums come from downloading the release assets; the archive was never unpacked into a Flatpak build. Update the tag and hashes whenever `cargo update` moves `webrtc-sys`. Also unconfirmed: that an `llvm21` SDK extension exists for the runtime's freedesktop branch (clang 21 or later is required), that the SDK needs the `blueprint-compiler` module, and the build time and memory of the LTO release profile. A Flathub submission would also need screenshots and a git source with a commit instead of the local directory.

## Checks

CI runs these through `apps/linux/env.cue`:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
desktop-file-validate data/social.waddle.Waddle.desktop
appstreamcli validate --no-net data/social.waddle.Waddle.metainfo.xml
```

The GTK window tests require a display and are excluded from ordinary headless runs. GTK can start once per process, so run each separately: `xvfb-run -a cargo test --locked thread_navigation_and_context -- --ignored --test-threads=1`, then the same with `profile_surfaces_open_and_resolve_authors` (the profile, People and profile-sheet dialogs, built against a session that cannot connect) and `the_call_window_follows_the_slot` (the call window opening for a ringing call and closing when it is declined), `the_window_names_its_controls_and_wires_its_shortcuts` (shortcut actions exist, the shortcuts window opens, icon buttons have tooltips and at least 24 px), `the_style_sheet_parses_without_errors_in_both_schemes` and `every_accelerator_parses`.

## Layout

| Path | What |
|---|---|
| `src/auth/device.rs` | Device-flow client; typed server URL, session id and JID; tests against a local stand-in server |
| `src/auth/store.rs` | Secret Service account store |
| `src/runtime.rs` | tokio on its own threads; UI awaits work from the GLib main loop |
| `src/message_actions.rs`, `src/pins.rs`, `src/jump.rs`, `src/message_text.rs` | Message action rules, the pinned list, jump planning and body rendering, without widgets |
| `src/session_tools.rs`, `src/ui/message_tools.rs` | Pin, search and moderation verbs; header buttons, search bar, hover toolbar |
| `src/rooms.rs`, `src/session_rooms.rs`, `src/ui/rooms.rs` | Room rules (addresses, setting changes, who may moderate whom, member list, group DM members), the room and group DM verbs, and the dialogs |
| `src/delivery.rs`, `src/typing.rs`, `src/read_sync.rs`, `src/reconnect.rs` | The unsent queue and delivery states, chat states, read markers and cursors, and the reconnect state machine: no widgets, with tests |
| `src/session_link.rs`, `src/ui/delivery.rs`, `typing.rs`, `read_markers.rs`, `connection.rs` | Their session verbs and window glue: the queue, typing line, marker dispatch, banner, focus and network triggers |
| `src/composer.rs` | `@` mention and slash command suggestions and actions (`/me`, `/shrug`, `/away`, `/active`, `/dnd`) |
| `src/markup.rs` | XEP-0394: markdown markers to spans, spans to markers, the formatting bar's edits, the span index |
| `src/extension_commands.rs` | XEP-0050 slash commands and XEP-0004 form rules |
| `src/presence.rs`, `src/avatars.rs`, `src/profile.rs`, `src/people.rs`, `src/nick_color.rs` | Presence mode and auto-away, avatar cache, profile fields and registries, the People listing, nick colors: all without widgets |
| `src/ui/status.rs`, `pictures.rs`, `profile.rs`, `person.rs`, `people.rs`, `identity.rs` | Their windows: presence plumbing, avatar loading and publishing, your profile, someone's profile sheet, the People view, author resolution |
| `src/ui/window.rs`, `data/ui/window.blp` | Window and sign-in steps (Blueprint template) |
| `src/call.rs`, `src/call_engine.rs`, `src/call_screen.rs` | Call states, the XEP-0353 reducer and tie-break, join-token check; the engine that decides sends, timers and media for one call slot; what the call window shows (no GTK); tests |
| `src/call_media.rs`, `src/session_calls.rs` | The LiveKit room, microphone and speakers (libwebrtc platform audio) and ICE servers; the call verbs and event translation over the FFI |
| `src/ui/calls.rs` | Header button, call window, incoming-call notification, and carrying out the engine's effects in order |
| `src/notify.rs` | Notification modes, XEP-0492 defaults and the announce decision (no GTK); tests |
| `src/ui/notifications.rs` | Header menu, sidebar mute state and GNotification actions |
| `src/ui/suggestions.rs` | The composer's suggestion popover |
| `src/ui/composer_view.rs` | The composer's text view and formatting bar |
| `src/ui/commands.rs` | Extension command discovery, running and the form dialog |
| `src/shortcuts.rs`, `src/ui/shortcuts.rs`, `src/ui/a11y.rs` | The shortcut table, the Keyboard Shortcuts window, quick switch and conversation stepping; accessible names and the phrases screen readers get |
| `data/` | Desktop entry, AppStream metadata, icon, style sheet (Polar Huddle tokens) |
| `flatpak/` | Flatpak manifest (unbuilt, see Flatpak) |
