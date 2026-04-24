# UI/UX Redesign Plan

Target: Slack/Discord-quality native iOS experience

## Bugs to Fix
- [ ] GIF picker: no search, can't paste GIFs into composer
- [ ] Messages don't appear on first channel load (need channel switch workaround)

## Mobile Shell Redesign (Slack-style)
- [ ] Sidebar: compact server controls for the single server space
- [ ] Channel list: collapsible sections (Channels, Direct Messages, Starred)
- [ ] Tab bar: Home, DMs, Activity/Notifications, More, Search
- [ ] Dark theme as default with proper contrast

## Message Timeline
- [ ] Proper message layout: avatar (left) + name/timestamp (right) + body below
- [ ] Message grouping: cluster consecutive same-sender within 5 min, hide avatar/name
- [ ] Reactions bar: horizontal pills below message (not inline text)
- [ ] Action bar on long-press: reply, react, edit, delete, copy
- [ ] Unread divider line ("New messages" separator)

## Composer
- [ ] Bottom-pinned composer bar with attachment/GIF/emoji buttons
- [ ] Rich text preview (bold, italic rendered inline)
- [ ] Paste support for images and GIFs
- [ ] Reply preview bar (already exists, polish styling)

## Channel Header
- [ ] Clean header: # channel-name, member count, search icon
- [ ] Swipe-from-right for member list drawer

## Colors & Typography
- [ ] Dark background (#1a1a2e or system dark)
- [ ] Accent color for mentions, links, unread badges
- [ ] SF Pro / system font with proper weight hierarchy
- [ ] Message text: 15pt, names: 13pt semibold, timestamps: 11pt
