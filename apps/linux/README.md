# Waddle for Linux

A native GTK4 + libadwaita client, in Rust. The plan is `claude/linux-client-plan.md` (in the Waddle Project) and ADR-0019 ("One client state layer in Rust").

## Status

The native client currently supports:

- **Sign-in:** RFC 8628 device flow against any Waddle server, the same flow the Apple and Android apps use. Pick a provider, approve in the browser, and the window continues by itself.
- **Accounts:** stored in the Secret Service (`oo7`), one item per server and JID, so several communities can be signed in at once.
- **Shell:** an adaptive `AdwNavigationSplitView` that collapses at phone widths.
- **Chat:** rooms and DMs, paged history, replies, reactions, edits, deletions and attachments.
- **Threads:** choose **Reply in Thread** below a message, or **Open Thread** from its message menu. Threads appear beneath their conversation in the sidebar, with separate unread counts and history. **Start Subthread** creates a nested thread; the back button returns to the main conversation. The original message appears above the replies when loaded. Thread views filter full archive pages so saved reactions, edits and Jev judgments remain available. Automatic scanning stops after three pages; **Load Earlier** continues through older history.
- **Jev badges:** question and content-safety judgments appear beneath messages. Expand a badge to inspect probabilities, model and taxonomy versions. Judgments follow the exact message revision and disappear after an edit or deletion until a current result arrives.

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

The GTK navigation regression test requires a display and is excluded from ordinary headless runs. Run it separately with `xvfb-run -a cargo test --locked thread_navigation_and_context -- --ignored --test-threads=1`.

## Layout

| Path | What |
|---|---|
| `src/auth/device.rs` | Device-flow client; typed server URL, session id and JID; tests against a local stand-in server |
| `src/auth/store.rs` | Secret Service account store |
| `src/runtime.rs` | tokio on its own threads; UI awaits work from the GLib main loop |
| `src/composer.rs` | `@` mention and slash command suggestions and actions (`/me`, `/shrug`, `/away`, `/active`, `/dnd`) |
| `src/ui/window.rs`, `data/ui/window.blp` | Window and sign-in steps (Blueprint template) |
| `src/ui/suggestions.rs` | The composer's suggestion popover |
| `data/` | Desktop entry, AppStream metadata, icon |
