# Waddle for Linux

A native GTK4 + libadwaita client, in Rust. The plan is `claude/linux-client-plan.md` (in the Waddle Project) and ADR-0019 ("One client state layer in Rust").

## Status

Scaffold only:

- **Sign-in:** RFC 8628 device flow against any Waddle server, the same flow the Apple and Android apps use. Pick a provider, approve in the browser, and the window continues by itself.
- **Accounts:** stored in the Secret Service (`oo7`), one item per server and JID, so several communities can be signed in at once.
- **Shell:** an adaptive `AdwNavigationSplitView` that collapses at phone widths.

Rooms, DMs, timelines, presence and calls arrive with `waddle-client-session` (ADR-0019). This app keeps no client state of its own.

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
