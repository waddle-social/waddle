# Mobile UI Redesign: Slack/Discord-Quality Chat

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transform the Waddle iOS mobile chat from glass-morphism prototype styling into a professional, dark-themed chat experience matching Slack and Discord design quality.

**Architecture:** The redesign touches three layers: (1) a new `WaddleTheme` color/spacing system replacing scattered opacity constants, (2) rewritten message row and composer views using Slack-style left-aligned avatar layout, (3) restructured mobile shell with a proper channel sidebar and tab bar. All changes are view-layer only; no model or XMPP changes needed.

**Tech Stack:** SwiftUI (iOS 17+), `@Environment` for theme injection, `AsyncImage` for avatars, `ScrollViewReader` for scroll anchoring.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `Chat/WaddleTheme.swift` | **Create** | Color tokens, spacing, typography, reusable style modifiers |
| `Chat/ChatViews.swift` | **Modify** | Rewrite `ChatMessageRowView`, `ChatComposerView`, `ChatTimelineView` |
| `Chat/ChatWorkspaceView.swift` | **Modify** | Rewrite compact layout: remove chrome header, inline channel sidebar |
| `App/MobileSlackShellView.swift` | **Modify** | Restructure tabs, add DM tab, simplify home tab |

---

### Task 1: Create WaddleTheme Color System

**Files:**
- Create: `apps/apple/Waddle/Chat/WaddleTheme.swift`

This replaces all the scattered `Color.secondary.opacity(0.08)` and `Color.accentColor.opacity(0.12)` patterns with named semantic tokens.

- [ ] **Step 1: Create the theme file**

```swift
import SwiftUI

enum WaddleTheme {
    // MARK: - Backgrounds
    static let chatBackground = Color(red: 0.07, green: 0.07, blue: 0.11)      // #121120 - main dark bg
    static let sidebarBackground = Color(red: 0.05, green: 0.05, blue: 0.09)   // #0D0D17 - darker sidebar
    static let surfaceRaised = Color(red: 0.11, green: 0.11, blue: 0.16)       // #1C1C29 - cards/header
    static let surfaceHover = Color(red: 0.14, green: 0.14, blue: 0.20)        // #242434 - hover/selected
    static let composerBackground = Color(red: 0.10, green: 0.10, blue: 0.15)  // #1A1A26

    // MARK: - Text
    static let textPrimary = Color.white
    static let textSecondary = Color(white: 0.62)         // #9E9E9E
    static let textMuted = Color(white: 0.40)             // #666666
    static let textLink = Color(red: 0.35, green: 0.55, blue: 1.0) // #5A8CFF

    // MARK: - Accent
    static let accent = Color(red: 0.37, green: 0.36, blue: 0.90)  // #5E5CE6 - Slack purple-blue
    static let mentionHighlight = Color(red: 0.35, green: 0.55, blue: 1.0).opacity(0.15)
    static let unreadBadge = Color.red

    // MARK: - Presence
    static let presenceOnline = Color(red: 0.18, green: 0.80, blue: 0.44)  // #2ECC71
    static let presenceAway = Color(red: 0.95, green: 0.77, blue: 0.06)    // #F1C40F
    static let presenceDnd = Color.red
    static let presenceOffline = Color(white: 0.40)

    // MARK: - Messages
    static let ownMessageBubble = Color(red: 0.37, green: 0.36, blue: 0.90).opacity(0.12)
    static let messageHover = Color.white.opacity(0.03)

    // MARK: - Dividers & Borders
    static let divider = Color.white.opacity(0.06)
    static let channelSelected = Color.white.opacity(0.08)

    // MARK: - Spacing
    static let messageAvatarSize: CGFloat = 36
    static let messageSpacingY: CGFloat = 2
    static let messageClusterSpacingY: CGFloat = 16
    static let composerHeight: CGFloat = 44
    static let sidebarWidth: CGFloat = 260

    // MARK: - Typography
    static let senderFont = Font.subheadline.weight(.semibold)
    static let timestampFont = Font.caption2
    static let bodyFont = Font.subheadline
    static let channelFont = Font.subheadline
}
```

- [ ] **Step 2: Commit**

```bash
git add apps/apple/Waddle/Chat/WaddleTheme.swift
git commit -m "feat(apple/ui): add WaddleTheme color/spacing system"
```

---

### Task 2: Rewrite ChatMessageRowView — Slack-Style Layout

**Files:**
- Modify: `apps/apple/Waddle/Chat/ChatViews.swift` (lines 331-831)

Replace the three-layout system (operationalRow/compactPhoneRow/bubbleRow) with a single Slack-style layout: 36px avatar on the left, sender name + timestamp on first line, message body below. Consecutive messages from the same sender within 5 minutes collapse (hide avatar + name, indent body).

- [ ] **Step 1: Replace ChatMessageRowView body and remove old layouts**

Replace the `body` computed property and remove `operationalRow`, `compactPhoneRow`, `bubbleRow`, `messageCard`, `compactMessageCard`. Replace with:

```swift
var body: some View {
    if message.isAction {
        actionRow
    } else {
        slackStyleRow
    }
}

private var actionRow: some View {
    Text(message.body)
        .font(.caption)
        .foregroundStyle(WaddleTheme.textMuted)
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .center)
}

@ViewBuilder
private var slackStyleRow: some View {
    let showsHeader = !message.formsCompactCluster(with: previousMessage)

    HStack(alignment: .top, spacing: 10) {
        if showsHeader {
            avatar
                .frame(width: WaddleTheme.messageAvatarSize, height: WaddleTheme.messageAvatarSize)
        } else {
            Color.clear
                .frame(width: WaddleTheme.messageAvatarSize, height: 1)
        }

        VStack(alignment: .leading, spacing: 3) {
            if showsHeader {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(message.senderDisplayName)
                        .font(WaddleTheme.senderFont)
                        .foregroundStyle(WaddleTheme.textPrimary)

                    Text(message.sentAt, style: .time)
                        .font(WaddleTheme.timestampFont)
                        .foregroundStyle(WaddleTheme.textMuted)

                    if message.editedAt != nil {
                        Text("(edited)")
                            .font(WaddleTheme.timestampFont)
                            .foregroundStyle(WaddleTheme.textMuted)
                    }

                    if let hats = message.hatTitles {
                        ForEach(hats, id: \.self) { hat in
                            Text(hat)
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(hatColor(for: hat))
                                .padding(.horizontal, 5)
                                .padding(.vertical, 1)
                                .background(hatColor(for: hat).opacity(0.15), in: Capsule())
                        }
                    }

                    if let mention = message.broadcastMention {
                        Text("@\(mention)")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 1)
                            .background(Color.orange, in: Capsule())
                    }
                }
            }

            if let replyToID = message.replyToID, !replyToID.isEmpty {
                replyIndicator
            }

            if message.isRetracted {
                Text("This message was deleted.")
                    .font(WaddleTheme.bodyFont)
                    .italic()
                    .foregroundStyle(WaddleTheme.textMuted)
            } else {
                if !message.displayBody.isEmpty {
                    Text(message.styledBody)
                        .font(WaddleTheme.bodyFont)
                        .foregroundStyle(WaddleTheme.textPrimary)
                        .textSelection(.enabled)
                }

                inlineImagesView(for: message, maxWidth: 300)
                downloadableFilesView(for: message)
            }

            if let reactions = message.reactions, !reactions.isEmpty {
                reactionBar(reactions)
            }
        }

        Spacer(minLength: 0)
    }
    .padding(.horizontal, 16)
    .padding(.top, showsHeader ? 8 : 1)
    .padding(.bottom, 1)
    .background(WaddleTheme.messageHover)
    .contextMenu {
        if !message.isAction, !message.isRetracted {
            if onReply != nil {
                Button { onReply?(message) } label: {
                    Label("Reply", systemImage: "arrowshape.turn.up.left")
                }
            }
            if message.isOutgoing, onRetract != nil {
                Button(role: .destructive) { onRetract?(message) } label: {
                    Label("Delete", systemImage: "trash")
                }
            }
        }
    }
}

private func reactionBar(_ reactions: [String: [String]]) -> some View {
    HStack(spacing: 4) {
        ForEach(reactions.keys.sorted(), id: \.self) { emoji in
            let count = reactions[emoji]?.count ?? 0
            HStack(spacing: 3) {
                Text(emoji).font(.caption)
                Text("\(count)").font(.caption2.weight(.medium)).foregroundStyle(WaddleTheme.textSecondary)
            }
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(WaddleTheme.divider))
        }
    }
}
```

- [ ] **Step 2: Update replyIndicator styling**

Replace the existing `replyIndicator` with dark-theme styling:

```swift
@ViewBuilder
private var replyIndicator: some View {
    HStack(spacing: 6) {
        RoundedRectangle(cornerRadius: 1.5)
            .fill(WaddleTheme.accent)
            .frame(width: 2)

        VStack(alignment: .leading, spacing: 1) {
            if let senderName = message.replyToSenderName, !senderName.isEmpty {
                Text(senderName)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(WaddleTheme.accent)
            }
            Text(message.replyToBody ?? "Original message")
                .font(.caption2)
                .foregroundStyle(WaddleTheme.textMuted)
                .lineLimit(1)
        }
    }
    .padding(.leading, 8)
    .padding(.vertical, 4)
}
```

- [ ] **Step 3: Update avatar to use accent-hued background**

```swift
private var avatar: some View {
    Text(message.senderInitials ?? initials(from: message.senderDisplayName))
        .font(.caption.weight(.bold))
        .foregroundStyle(.white)
        .frame(width: WaddleTheme.messageAvatarSize, height: WaddleTheme.messageAvatarSize)
        .background(
            WaddleTheme.accent.opacity(0.6),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
}
```

- [ ] **Step 4: Build and verify**

```bash
xcodebuild -project apps/apple/Waddle.xcodeproj -scheme Waddle -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build 2>&1 | grep -E 'error:|BUILD' | tail -5
```

Expected: BUILD SUCCEEDED

- [ ] **Step 5: Commit**

```bash
git add apps/apple/Waddle/Chat/ChatViews.swift
git commit -m "feat(apple/ui): rewrite message rows to Slack-style layout"
```

---

### Task 3: Rewrite ChatComposerView — Clean Bottom Bar

**Files:**
- Modify: `apps/apple/Waddle/Chat/ChatViews.swift` (ChatComposerView, lines ~833-1221)

Replace the two-layout composer (standard + compact) with a single clean bottom bar: dark input field with attachment/GIF/emoji buttons on the left, send on the right. Reply preview bar above when active.

- [ ] **Step 1: Replace composer body**

Remove `standardComposer` and `compactConversationComposer` properties. Replace the `body` with:

```swift
var body: some View {
    VStack(spacing: 0) {
        mentionSuggestionList

        composerReplyPreview

        HStack(alignment: .bottom, spacing: 8) {
            HStack(spacing: 4) {
                attachmentPickerButton
                gifPickerButton
                emojiPickerButton
            }

            ZStack(alignment: .leading) {
                if !hasSendableText {
                    Text(placeholder)
                        .foregroundStyle(WaddleTheme.textMuted)
                        .padding(.leading, 12)
                }

                TextEditor(text: $text)
                    .scrollContentBackground(.hidden)
                    .foregroundStyle(WaddleTheme.textPrimary)
                    .font(WaddleTheme.bodyFont)
                    .frame(minHeight: 36, maxHeight: 120)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
            }
            .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(WaddleTheme.divider))

            Button(action: onSend) {
                Image(systemName: "paperplane.fill")
                    .font(.body)
                    .foregroundStyle(canSend && hasSendableText ? WaddleTheme.accent : WaddleTheme.textMuted)
            }
            .disabled(!canSend || isSending || !hasSendableText)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(WaddleTheme.composerBackground)
    }
    .onChange(of: text) { _, newValue in
        updateMentionQuery(newValue)
    }
}
```

- [ ] **Step 2: Restyle reply preview**

```swift
@ViewBuilder
private var composerReplyPreview: some View {
    if let reply = replyingToMessage {
        HStack(spacing: 8) {
            RoundedRectangle(cornerRadius: 1.5)
                .fill(WaddleTheme.accent)
                .frame(width: 2)

            VStack(alignment: .leading, spacing: 2) {
                Text("Replying to \(reply.senderDisplayName)")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(WaddleTheme.accent)
                Text(reply.body)
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textSecondary)
                    .lineLimit(1)
            }

            Spacer()

            Button { onCancelReply?() } label: {
                Image(systemName: "xmark")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(WaddleTheme.surfaceRaised)
    }
}
```

- [ ] **Step 3: Restyle action buttons**

Update `attachmentPickerButton`, `gifPickerButton`, `emojiPickerButton` to use consistent icon-only dark-theme styling:

```swift
private var attachmentPickerButton: some View {
    // ... keep PhotosPicker logic but change visual:
    .foregroundStyle(WaddleTheme.textSecondary)
}

private var gifPickerButton: some View {
    Button { showGifPicker.toggle() } label: {
        Image(systemName: "play.rectangle")
            .font(.body)
            .foregroundStyle(WaddleTheme.textSecondary)
    }
    .buttonStyle(.plain)
    .popover(isPresented: $showGifPicker) { ... }
}

private var emojiPickerButton: some View {
    Button { showEmojiPicker.toggle() } label: {
        Image(systemName: "face.smiling")
            .font(.body)
            .foregroundStyle(WaddleTheme.textSecondary)
    }
    .buttonStyle(.plain)
    .popover(isPresented: $showEmojiPicker) { ... }
}
```

- [ ] **Step 4: Build and verify**

```bash
xcodebuild -project apps/apple/Waddle.xcodeproj -scheme Waddle -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build 2>&1 | grep -E 'error:|BUILD' | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add apps/apple/Waddle/Chat/ChatViews.swift
git commit -m "feat(apple/ui): rewrite composer to clean dark bottom bar"
```

---

### Task 4: Rewrite Compact Layout — Channel Sidebar + Chat Area

**Files:**
- Modify: `apps/apple/Waddle/Chat/ChatWorkspaceView.swift` (compact layout, lines ~234-512)

Replace the "compact chrome" header (waddle name + horizontal channel pills + members button) with a proper channel sidebar that slides in from the left, and a clean chat area with a minimal header bar showing `# channel-name`.

- [ ] **Step 1: Replace compactLayoutView**

Remove `compactChrome`, `compactHeader`, `compactChannelRail`, `compactChannelMenu`, `compactJoinButton`. Replace `compactLayoutView` with:

```swift
private var compactLayoutView: some View {
    ZStack(alignment: .leading) {
        // Main chat content
        VStack(spacing: 0) {
            compactChatHeader
            conversationPane(compactStyle: true)
        }
        .background(WaddleTheme.chatBackground)

        // Channel sidebar overlay (swipe or button to toggle)
        if showChannelSidebar {
            Color.black.opacity(0.4)
                .ignoresSafeArea()
                .onTapGesture { showChannelSidebar = false }

            compactSidebar
                .frame(width: WaddleTheme.sidebarWidth)
                .background(WaddleTheme.sidebarBackground)
                .transition(.move(edge: .leading))
        }
    }
    .animation(.easeOut(duration: 0.2), value: showChannelSidebar)
}
```

Add `@State private var showChannelSidebar = false` to the view.

- [ ] **Step 2: Create compact chat header**

```swift
private var compactChatHeader: some View {
    HStack(spacing: 12) {
        Button { showChannelSidebar.toggle() } label: {
            Image(systemName: "line.3.horizontal")
                .font(.body.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .buttonStyle(.plain)

        HStack(spacing: 6) {
            Image(systemName: "number")
                .font(.caption.weight(.bold))
                .foregroundStyle(WaddleTheme.textMuted)
            Text(store.selectedRoom?.title ?? "Select channel")
                .font(.headline)
                .foregroundStyle(WaddleTheme.textPrimary)
                .lineLimit(1)
        }

        Spacer()

        Button { showMembersSheet = true } label: {
            Image(systemName: "person.2")
                .font(.subheadline)
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .buttonStyle(.plain)
    }
    .padding(.horizontal, 16)
    .padding(.vertical, 10)
    .background(WaddleTheme.surfaceRaised)
    .overlay(alignment: .bottom) { WaddleTheme.divider.frame(height: 1) }
}
```

- [ ] **Step 3: Create compact sidebar**

```swift
private var compactSidebar: some View {
    VStack(alignment: .leading, spacing: 0) {
        // Waddle header
        HStack(spacing: 10) {
            Text(waddle.name.prefix(2).uppercased())
                .font(.caption.weight(.bold))
                .foregroundStyle(.white)
                .frame(width: 32, height: 32)
                .background(WaddleTheme.accent, in: RoundedRectangle(cornerRadius: 8))

            VStack(alignment: .leading, spacing: 1) {
                Text(waddle.name)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(WaddleTheme.textPrimary)
                Text("\(model.members.count) members")
                    .font(.caption2)
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            Spacer()
        }
        .padding(16)

        Divider().overlay(WaddleTheme.divider)

        // Channels section
        ScrollView {
            VStack(alignment: .leading, spacing: 2) {
                sidebarSectionHeader("Channels", action: { showCreateChannelSheet = true })

                ForEach(store.rooms) { room in
                    sidebarChannelRow(room: room)
                }

                if !store.dmConversations.isEmpty {
                    sidebarSectionHeader("Direct Messages", action: { showNewDmSheet = true })

                    ForEach(store.dmConversations) { convo in
                        sidebarDmRow(convo: convo)
                    }
                }
            }
            .padding(.vertical, 8)
        }
    }
}

private func sidebarSectionHeader(_ title: String, action: @escaping () -> Void) -> some View {
    HStack {
        Text(title)
            .font(.caption.weight(.semibold))
            .foregroundStyle(WaddleTheme.textMuted)
            .textCase(.uppercase)
        Spacer()
        Button(action: action) {
            Image(systemName: "plus")
                .font(.caption)
                .foregroundStyle(WaddleTheme.textMuted)
        }
        .buttonStyle(.plain)
    }
    .padding(.horizontal, 16)
    .padding(.vertical, 6)
}

private func sidebarChannelRow(room: ChatRoomSelection) -> some View {
    Button {
        Task { await model.selectChannel(room.id) }
        showChannelSidebar = false
    } label: {
        HStack(spacing: 8) {
            Image(systemName: "number")
                .font(.caption2.weight(.bold))
                .foregroundStyle(model.selectedChannelID == room.id ? WaddleTheme.textPrimary : WaddleTheme.textMuted)
                .frame(width: 18)
            Text(room.title)
                .font(WaddleTheme.channelFont)
                .foregroundStyle(model.selectedChannelID == room.id ? WaddleTheme.textPrimary : WaddleTheme.textSecondary)
                .lineLimit(1)
            Spacer()
            if room.unreadCount > 0 {
                Text("\(room.unreadCount)")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(WaddleTheme.unreadBadge, in: Capsule())
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .background(model.selectedChannelID == room.id ? WaddleTheme.channelSelected : Color.clear, in: RoundedRectangle(cornerRadius: 6))
        .padding(.horizontal, 8)
    }
    .buttonStyle(.plain)
}

private func sidebarDmRow(convo: DmConversation) -> some View {
    Button {
        Task { await model.openDm(peerJID: convo.peerJID, peerUsername: convo.peerUsername) }
        showChannelSidebar = false
    } label: {
        HStack(spacing: 8) {
            Circle()
                .fill(convo.presenceShow == .available ? WaddleTheme.presenceOnline : WaddleTheme.presenceOffline)
                .frame(width: 8, height: 8)
            Text(convo.peerUsername)
                .font(WaddleTheme.channelFont)
                .foregroundStyle(WaddleTheme.textSecondary)
                .lineLimit(1)
            Spacer()
            if convo.unreadCount > 0 {
                Text("\(convo.unreadCount)")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(WaddleTheme.unreadBadge, in: Capsule())
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .padding(.horizontal, 8)
    }
    .buttonStyle(.plain)
}
```

- [ ] **Step 4: Build and verify**

```bash
xcodebuild -project apps/apple/Waddle.xcodeproj -scheme Waddle -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build 2>&1 | grep -E 'error:|BUILD' | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add apps/apple/Waddle/Chat/ChatWorkspaceView.swift
git commit -m "feat(apple/ui): rewrite compact layout with slide-out channel sidebar"
```

---

### Task 5: Update Timeline Background and Scroll

**Files:**
- Modify: `apps/apple/Waddle/Chat/ChatViews.swift` (ChatTimelineView)

- [ ] **Step 1: Update timeline view background and spacing**

Update `ChatTimelineView` to use dark theme background and remove operational density flags:

```swift
var body: some View {
    ScrollView {
        LazyVStack(alignment: .leading, spacing: 0) {
            // ... existing ForEach logic stays the same
            // but update spacing between messages
        }
        .padding(.vertical, 8)
    }
    .background(WaddleTheme.chatBackground)
}
```

- [ ] **Step 2: Update day divider**

```swift
struct ChatTimelineDayDividerView: View {
    let date: Date

    var body: some View {
        HStack(spacing: 12) {
            Rectangle().fill(WaddleTheme.divider).frame(height: 1)
            Text(date, format: .dateTime.weekday(.abbreviated).month(.abbreviated).day())
                .font(.caption2.weight(.semibold))
                .foregroundStyle(WaddleTheme.textMuted)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(WaddleTheme.surfaceRaised, in: Capsule())
            Rectangle().fill(WaddleTheme.divider).frame(height: 1)
        }
        .padding(.vertical, 12)
        .padding(.horizontal, 16)
    }
}
```

- [ ] **Step 3: Update typing indicator**

```swift
struct ChatTypingIndicatorView: View {
    let typingUsers: [String]

    var body: some View {
        if !typingUsers.isEmpty {
            HStack(spacing: 6) {
                ProgressView()
                    .scaleEffect(0.6)
                    .tint(WaddleTheme.textMuted)
                Text(typingLabel)
                    .font(.caption2)
                    .foregroundStyle(WaddleTheme.textMuted)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
        }
    }
    // ... keep typingLabel computed property
}
```

- [ ] **Step 4: Update empty/loading/error state views**

```swift
struct ChatEmptyStateView: View {
    var title: String
    var message: String?
    var systemImage: String = "bubble.left.and.bubble.right"

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: systemImage)
                .font(.title2)
                .foregroundStyle(WaddleTheme.textMuted)
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
            if let message {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textMuted)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}
```

- [ ] **Step 5: Build, verify, commit**

```bash
xcodebuild -project apps/apple/Waddle.xcodeproj -scheme Waddle -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build 2>&1 | grep -E 'error:|BUILD' | tail -5
git add apps/apple/Waddle/Chat/ChatViews.swift
git commit -m "feat(apple/ui): dark theme timeline, dividers, states"
```

---

### Task 6: Simplify Mobile Shell Tabs

**Files:**
- Modify: `apps/apple/Waddle/App/MobileSlackShellView.swift`

Restructure the tab bar to: **Home** (channels + DMs when waddle selected), **DMs** (DM list), **Activity** (placeholder), **You** (settings). Remove Browse tab and glass morphism background.

- [ ] **Step 1: Update tab enum and tab view**

Change `MobileShellTab` to:
```swift
private enum MobileShellTab: String, Hashable {
    case home
    case dms
    case activity
    case you
}
```

Update the `TabView` body to use the new tabs. The `home` tab should show `MobileConversationTab` (the chat view) since that's where users spend time. The `dms` tab shows a DM list.

- [ ] **Step 2: Replace MobileShellBackground**

Replace the glass morphism gradient with a simple dark background:
```swift
private struct MobileShellBackground: View {
    var body: some View {
        WaddleTheme.chatBackground.ignoresSafeArea()
    }
}
```

- [ ] **Step 3: Build, verify, commit**

```bash
xcodebuild -project apps/apple/Waddle.xcodeproj -scheme Waddle -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build 2>&1 | grep -E 'error:|BUILD' | tail -5
git add apps/apple/Waddle/App/MobileSlackShellView.swift
git commit -m "feat(apple/ui): simplify mobile tabs, dark background"
```

---

### Task 7: Fix GIF Picker Search and Polish

**Files:**
- Modify: `apps/apple/Waddle/Chat/ChatViews.swift` (ChatGifPickerView)

- [ ] **Step 1: Fix GIF picker to use dark theme and improve search**

```swift
struct ChatGifPickerView: View {
    var onSelect: (String) -> Void
    @State private var searchText = ""
    @State private var results: [GiphyGif] = []
    @State private var isLoading = false
    @State private var searchTask: Task<Void, Never>?

    // ... keep existing GiphyGif struct and GiphyResponse

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(WaddleTheme.textMuted)
                TextField("Search GIFs", text: $searchText)
                    .foregroundStyle(WaddleTheme.textPrimary)
                if !searchText.isEmpty {
                    Button { searchText = "" } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(WaddleTheme.textMuted)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(10)
            .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 8))
            .padding(10)
            .onChange(of: searchText) { _, query in
                searchTask?.cancel()
                searchTask = Task {
                    try? await Task.sleep(nanoseconds: 300_000_000)
                    guard !Task.isCancelled else { return }
                    await fetchGifs(query: query)
                }
            }

            // ... keep existing grid/loading/empty content
        }
        .frame(width: 360, height: 380)
        .background(WaddleTheme.sidebarBackground)
        .task { await fetchGifs(query: "") }
    }

    // ... keep fetchGifs function
}
```

- [ ] **Step 2: Build, verify, commit**

```bash
xcodebuild -project apps/apple/Waddle.xcodeproj -scheme Waddle -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build 2>&1 | grep -E 'error:|BUILD' | tail -5
git add apps/apple/Waddle/Chat/ChatViews.swift
git commit -m "feat(apple/ui): dark theme GIF picker with improved search"
```

---

### Task 8: Deploy and Verify on Device

**Files:** None (deployment only)

- [ ] **Step 1: Build for device**

```bash
xcodebuild -project apps/apple/Waddle.xcodeproj -scheme Waddle -destination 'id=00008150-0016486E1E02401C' build 2>&1 | grep -E 'error:|BUILD' | tail -5
```

- [ ] **Step 2: Install on device**

```bash
xcrun devicectl device install app --device 00008150-0016486E1E02401C /Users/rawkode/Library/Developer/Xcode/DerivedData/Waddle-caxazlfnebznpygwzffyumbdjbkb/Build/Products/Debug-iphoneos/Waddle.app 2>&1 | grep 'installed'
```

- [ ] **Step 3: Commit all and push**

```bash
git add -A
git commit -m "feat(apple/ui): complete Slack/Discord-style UI redesign"
git push origin main
```
