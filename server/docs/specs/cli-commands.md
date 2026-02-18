# CLI TUI Specification

## Overview

This document specifies the Waddle Social CLI TUI client, including commands, keybindings, and UI layout.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│ Waddle CLI TUI                                                  │
├─────────────────────────────────────────────────────────────────┤
│ ┌─────────────┐ ┌─────────────────────────────────────────────┐ │
│ │             │ │                                             │ │
│ │   Sidebar   │ │              Message View                   │ │
│ │             │ │                                             │ │
│ │  - Waddles  │ │  [10:30] alice: Hello everyone!             │ │
│ │  - Channels │ │  [10:31] bob: Hey! How's it going?          │ │
│ │  - DMs      │ │  [10:32] alice: Great! Working on Waddle    │ │
│ │             │ │                                             │ │
│ │             │ │                                             │ │
│ └─────────────┘ └─────────────────────────────────────────────┘ │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ > Type a message... (Enter to send, Esc to cancel)         │ │
│ └─────────────────────────────────────────────────────────────┘ │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ 🟢 online | #general | penguin-club | ?:help               │ │
│ └─────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

## Layout Panels

### Sidebar (Left)

```
Waddles
├── 🐧 penguin-club
│   ├── # general
│   ├── # announcements (3)
│   └── # random
├── 🎮 gamers-unite
│   ├── # lobby
│   └── # minecraft
└── + Join/Create

DMs
├── 👤 alice.bsky.social (2)
├── 👥 Project Team
└── + New DM
```

- Collapsible Waddle trees
- Unread counts in parentheses
- Bold for mentions

### Message View (Main)

```
#general in penguin-club                     12 members online
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[10:30] alice (Alice)
  Hello everyone! Check out this **cool** thing:
  ```rust
  fn main() {
      println!("🐧");
  }
  ```

[10:31] bob (Bob)
  > Hello everyone!
  Nice! 🎉

[10:32] alice (Alice)
  @bob thanks! Here's a screenshot:
  📎 screenshot.png (245 KB) [View]

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
                                                   Scroll: 85%
```

- Inline markdown rendering
- Syntax-highlighted code blocks
- Reply context (quoted)
- Attachment indicators

### Input Area

```
┌─────────────────────────────────────────────────────────────┐
│ > Replying to alice:                                        │
│ > This is my **formatted** message with `code`              │
│                                                        [2/3]│
└─────────────────────────────────────────────────────────────┘
```

- Multi-line input
- Reply indicator
- Character count warning

### Status Bar

```
🟢 online | #general | penguin-club | ↑↓:scroll j/k:nav ?:help
```

- Connection status
- Current context
- Key hints

## Keybindings

### Global

| Key | Action |
|-----|--------|
| `?` | Show help overlay |
| `Ctrl+c` | Quit |
| `Ctrl+l` | Refresh screen |
| `Esc` | Cancel / Back |
| `Tab` | Cycle focus |
| `:` | Command mode |

### Navigation (Vim-style)

| Key | Action |
|-----|--------|
| `j` / `↓` | Next item |
| `k` / `↑` | Previous item |
| `h` / `←` | Collapse / Back |
| `l` / `→` | Expand / Enter |
| `g` | Go to top |
| `G` | Go to bottom |
| `Ctrl+d` | Page down |
| `Ctrl+u` | Page up |
| `/` | Search |
| `n` | Next search result |
| `N` | Previous search result |

### Sidebar

| Key | Action |
|-----|--------|
| `Enter` | Select channel/DM |
| `Space` | Expand/collapse Waddle |
| `a` | Mark all as read |
| `m` | Mute/unmute |
| `J` | Join Waddle |
| `C` | Create channel |
| `D` | Start DM |

### Message View

| Key | Action |
|-----|--------|
| `Enter` | Start composing |
| `r` | Reply to selected |
| `e` | Edit own message |
| `d` | Delete own message |
| `p` | Pin/unpin message |
| `+` | Add reaction |
| `y` | Copy message |
| `o` | Open link/attachment |
| `t` | Open thread |

### Input

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Shift+Enter` | New line |
| `Esc` | Cancel input |
| `Ctrl+u` | Clear line |
| `Ctrl+a` | Beginning of line |
| `Ctrl+e` | End of line |
| `↑` | Edit last message |
| `Tab` | Autocomplete |

## Command Mode

Enter command mode with `:`. Commands:

### Connection

```
:connect              Reconnect to server
:disconnect           Disconnect
:status               Show connection status
```

### Navigation

```
:join <invite>        Join Waddle by invite
:leave                Leave current Waddle
:channel <name>       Switch to channel
:dm <handle>          Open DM with user
```

### Messaging

```
:send                 Send message (alias for Enter)
:reply <msg_id>       Reply to specific message
:edit <msg_id>        Edit message
:delete <msg_id>      Delete message
```

### User

```
:status <text>        Set status message
:presence <state>     Set online/idle/dnd/invisible
:profile              View/edit profile
```

### Settings

```
:set <key> <value>    Change setting
:theme <name>         Switch theme
:keybind <key> <cmd>  Custom keybinding
```

### Utility

```
:help [command]       Show help
:quit                 Exit application
:version              Show version info
:debug                Toggle debug mode
```

## Configuration

### Config File

`~/.config/waddle/config.toml`:

```toml
[general]
theme = "dark"
timestamps = "relative"      # relative, absolute, none
notifications = true

[keybindings]
quit = "ctrl+q"
send = "ctrl+enter"

[display]
sidebar_width = 25
show_avatars = false
compact_mode = false
message_grouping = 300       # seconds to group messages

[notifications]
sound = true
desktop = true
mentions_only = false

[presence]
auto_idle = 300              # seconds before auto-idle
show_typing = true
```

### Themes

Built-in themes:
- `dark` (default)
- `light`
- `nord`
- `dracula`
- `solarized`

Custom themes in `~/.config/waddle/themes/`:

```toml
# custom.toml
[colors]
background = "#1a1b26"
foreground = "#a9b1d6"
primary = "#7aa2f7"
secondary = "#bb9af7"
success = "#9ece6a"
warning = "#e0af68"
error = "#f7768e"
muted = "#565f89"

[message]
author = "primary"
timestamp = "muted"
content = "foreground"
mention = "warning"
```

## Startup Flags

```
waddle-cli [OPTIONS]

Options:
  -c, --config <PATH>     Config file path
  -s, --server <URL>      Server URL override
  -t, --token <TOKEN>     Auth token (or use WADDLE_TOKEN env)
  -d, --debug             Enable debug logging
  -v, --verbose           Verbose output
  --no-color              Disable colors
  --help                  Show help
  --version               Show version
```

## Features

### Markdown Rendering

Inline rendering in terminal:
- **Bold** → Bold (with bold attribute)
- *Italic* → Italic (with italic attribute)
- `Code` → Highlighted background
- Code blocks → Syntax highlighted
- Links → Underlined, clickable (terminal-dependent)
- Lists → Proper indentation
- Quotes → Left border

### Autocomplete

Trigger with `Tab`:
- `@` → User mention
- `#` → Channel link
- `:` → Emoji
- `/` → Slash command

### Notifications

Desktop notifications via:
- `notify-send` (Linux)
- `terminal-notifier` (macOS)
- `toast` (Windows)

### Image Viewing

For image attachments:
- `o` opens in system viewer
- Sixel/iTerm2/Kitty protocols for inline preview (if supported)

## State Management

### Local Cache

`~/.cache/waddle/`:
- `messages/` - Recent message cache
- `media/` - Downloaded attachments
- `state.json` - UI state (collapsed channels, etc.)

### Session

`~/.local/share/waddle/`:
- `session.json` - Auth tokens
- `history` - Command history

## Related

- [ADR-0003: Ratatui CLI](../adrs/0003-ratatui-cli.md)
- [Spec: XMPP Integration](./xmpp-integration.md)
- [RFC-0004: Rich Message Format](../rfcs/0004-message-format.md)
