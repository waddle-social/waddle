import SwiftUI

struct WaddleChatWorkspaceView: View {
    @ObservedObject var model: AppModel
    @ObservedObject var store: ChatSurfaceStore
    let waddle: WaddleSummary
    @State private var showMembersSheet = false

    var body: some View {
        GeometryReader { proxy in
            let compactLayout = proxy.size.width < 760
            let showMembersInline = proxy.size.width > 1040

            Group {
                if compactLayout {
                    compactLayoutView
                } else {
                    regularLayout(showMembersInline: showMembersInline)
                }
            }
            .background(workspaceBackground.ignoresSafeArea())
        }
        .sheet(isPresented: $showMembersSheet) {
            NavigationStack {
                memberPane
                    .navigationTitle("Members")
#if os(iOS)
                    .navigationBarTitleDisplayMode(.inline)
#endif
            }
            .presentationDetents([.medium, .large])
        }
    }

    private var compactLayoutView: some View {
        VStack(spacing: 0) {
            compactChrome
            conversationPane(compactStyle: true)
        }
        .background(workspaceBackground)
    }

    @ViewBuilder
    private func regularLayout(showMembersInline: Bool) -> some View {
#if os(macOS)
        HStack(spacing: 16) {
            desktopChannelRail
                .frame(minWidth: 260, idealWidth: 280, maxWidth: 310)

            desktopConversationPane(showMembersInline: showMembersInline)
                .frame(minWidth: 620, maxWidth: .infinity, maxHeight: .infinity)

            if showMembersInline {
                desktopMemberPane
                    .frame(width: 280)
            }
        }
        .padding(18)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
#else
        VStack(spacing: 0) {
            compactChrome

            HStack(spacing: 0) {
                desktopChannelRail
                    .frame(minWidth: 220, idealWidth: 240, maxWidth: 280)

                Divider()

                conversationPane(compactStyle: false)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                if showMembersInline {
                    Divider()
                    memberPane
                        .frame(width: 240)
                }
            }
        }
#endif
    }

    private var compactChrome: some View {
        VStack(spacing: 0) {
            compactHeader
            compactChannelRail
        }
        .background(compactChromeBackground)
        .overlay(alignment: .bottom) {
            Divider()
        }
    }

    private var compactHeader: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .center, spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(waddle.name)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)

                    if store.selectedRoom == nil, let description = waddle.description, !description.isEmpty {
                        Text(description)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }

                Spacer(minLength: 8)

                compactChannelMenu

                Button {
                    showMembersSheet = true
                } label: {
                    Image(systemName: "person.2.fill")
                        .font(.subheadline.weight(.semibold))
                        .frame(width: 38, height: 38)
                        .background(compactToolbarFill, in: Circle())
                        .overlay(alignment: .topTrailing) {
                            if !model.chatMembers.isEmpty {
                                Text("\(min(model.chatMembers.count, 99))")
                                    .font(.caption2.weight(.bold))
                                    .foregroundStyle(.background)
                                    .padding(.horizontal, 5)
                                    .padding(.vertical, 2)
                                    .background(Color.accentColor, in: Capsule())
                                    .offset(x: 6, y: -5)
                            }
                        }
                }
                .buttonStyle(.plain)

                compactJoinButton
            }

            if let room = store.selectedRoom {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text("#\(room.title)")
                            .font(.title3.weight(.semibold))
                            .lineLimit(1)

                        if room.unreadCount > 0 {
                            compactMetaPill(text: "\(room.unreadCount) new", systemImage: "circle.fill", tint: .accentColor)
                        }

                        Spacer(minLength: 0)
                    }

                    HStack(spacing: 8) {
                        if let subtitle = room.subtitle, !subtitle.isEmpty {
                            compactMetaPill(text: subtitle, systemImage: "bubble.left.and.text.bubble.right")
                        }

                        compactMetaPill(text: "\(model.chatMembers.count)", systemImage: "person.2.fill")

                        if let lastActivityAt = room.lastActivityAt {
                            compactMetaPill(
                                text: RelativeDateTimeFormatter().localizedString(for: lastActivityAt, relativeTo: Date()),
                                systemImage: "clock"
                            )
                        }
                    }

                    if store.bannerState.isVisible {
                        ChatConnectionBannerView(state: store.bannerState)
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Choose a channel")
                        .font(.title3.weight(.semibold))

                    if let description = waddle.description, !description.isEmpty {
                        Text(description)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                }
            }

            if store.selectedRoom == nil, store.bannerState.isVisible {
                ChatConnectionBannerView(state: store.bannerState)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 10)
    }

    private var joinButton: some View {
        Group {
            if model.isJoined(waddle.id) {
                Label("Joined", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            } else {
                Button("Join") {
                    Task { await model.join(waddle) }
                }
                .buttonStyle(.borderedProminent)
            }
        }
    }

    private var compactJoinButton: some View {
        Group {
            if model.isJoined(waddle.id) {
                Label("Joined", systemImage: "checkmark.circle.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .background(compactToolbarFill, in: Capsule())
            } else {
                Button("Join") {
                    Task { await model.join(waddle) }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            }
        }
    }

    private var compactChannelMenu: some View {
        Menu {
            if store.rooms.isEmpty {
                Text("No channels yet")
            } else {
                ForEach(store.rooms) { room in
                    Button {
                        Task { await model.selectChannel(room.id) }
                    } label: {
                        if room.id == model.selectedChannelID {
                            Label("#\(room.title)", systemImage: "checkmark")
                        } else {
                            Text("#\(room.title)")
                        }
                    }
                }
            }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "number")
                    .font(.caption.weight(.semibold))
                Text(store.selectedRoom?.title ?? "Channels")
                    .font(.footnote.weight(.semibold))
                    .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.caption2.weight(.bold))
            }
            .foregroundStyle(.primary)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(compactToolbarFill, in: Capsule())
        }
    }

    private var compactChannelRail: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(store.rooms) { room in
                        Button {
                            Task { await model.selectChannel(room.id) }
                        } label: {
                            HStack(spacing: 6) {
                                Text("#\(room.title)")
                                    .font(.footnote.weight(.semibold))
                                    .lineLimit(1)

                                if room.unreadCount > 0 {
                                    Text("\(room.unreadCount)")
                                        .font(.caption2.weight(.bold))
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 3)
                                        .background(Color.accentColor.opacity(0.16), in: Capsule())
                                }
                            }
                            .foregroundStyle(room.id == model.selectedChannelID ? .primary : .secondary)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 10)
                            .background(compactRailFill(isSelected: room.id == model.selectedChannelID), in: Capsule())
                            .overlay {
                                Capsule()
                                    .strokeBorder(compactRailStroke(isSelected: room.id == model.selectedChannelID), lineWidth: 1)
                            }
                        }
                        .buttonStyle(.plain)
                        .id(room.id)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 12)
            }
            .onAppear {
                scrollToSelectedChannel(using: proxy)
            }
            .onChange(of: model.selectedChannelID) { _, _ in
                scrollToSelectedChannel(using: proxy)
            }
        }
    }

    private var desktopChannelRail: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 12) {
                Text("Workspace")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .textCase(.uppercase)

                HStack(alignment: .top, spacing: 12) {
                    RoundedRectangle(cornerRadius: 16, style: .continuous)
                        .fill(Color.accentColor.opacity(0.14))
                        .frame(width: 44, height: 44)
                        .overlay {
                            Image(systemName: "bubble.left.and.bubble.right.fill")
                                .font(.headline.weight(.semibold))
                                .foregroundColor(Color.accentColor)
                        }

                    VStack(alignment: .leading, spacing: 4) {
                        Text(waddle.name)
                            .font(.title3.weight(.semibold))
                            .lineLimit(2)

                        if let description = waddle.description, !description.isEmpty {
                            Text(description)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                                .lineLimit(3)
                        }
                    }

                    Spacer(minLength: 0)
                }

                HStack(spacing: 8) {
                    desktopSidebarStat(title: "Channels", value: store.rooms.count)
                    desktopSidebarStat(title: "People", value: model.members.count)
                }

                HStack(spacing: 10) {
                    joinButton
                    Spacer(minLength: 0)
                    if model.isLoadingStructure {
                        Label("Syncing", systemImage: "arrow.triangle.2.circlepath")
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .padding(18)
            .background(desktopPaneBackground)

            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .center) {
                    Text("Channels")
                        .font(.headline)
                    Spacer()
                    Text("\(store.rooms.count)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(.quaternary.opacity(0.7), in: Capsule())
                }

                Text("Focused rooms with live activity and quick switching.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 18)
            .padding(.top, 2)

            if store.rooms.isEmpty {
                ChatEmptyStateView(
                    title: "No channels yet",
                    message: "Join this waddle or wait for live discovery to finish.",
                    systemImage: "number"
                )
                .padding(.horizontal, 18)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 8) {
                        ForEach(store.rooms) { room in
                            ChatDesktopChannelRowView(
                                room: room,
                                isSelected: model.selectedChannelID == room.id
                            ) {
                                Task { await model.selectChannel(room.id) }
                            }
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 12)
                }
            }
        }
        .padding(12)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(desktopPaneBackground)
    }

    private func desktopConversationPane(showMembersInline: Bool) -> some View {
        VStack(spacing: 0) {
            ChatConversationHeaderView(
                room: store.selectedRoom,
                bannerState: store.bannerState,
                memberCount: model.chatMembers.count,
                messageCount: store.messages.count,
                showsMemberButton: !showMembersInline,
                onShowMembers: !showMembersInline ? { showMembersSheet = true } : nil,
                usesOperationalChrome: true
            )
            .padding(12)
            .padding(.bottom, 4)

            Divider()
                .padding(.horizontal, 12)

            desktopConversationContent
        }
        .background(desktopPaneBackground)
    }

    @ViewBuilder
    private var desktopConversationContent: some View {
        switch store.surfaceState {
        case .loading:
            ChatLoadingStateView(title: "Preparing conversation…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .empty(let title, let message):
            ChatEmptyStateView(title: title, message: message)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .error(let title, let message):
            ChatErrorStateView(title: title, message: message) {
                Task { await model.reloadSelectedWaddleStructure() }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .idle:
            VStack(spacing: 0) {
                ChatTimelineView(
                    messages: store.messages,
                    historyState: store.roomHistoryState,
                    onLoadOlderMessages: store.roomHistoryState.canLoadOlderMessages ? {
                        Task { await store.loadOlderMessages() }
                    } : nil,
                    onReply: { message in store.setReplyingTo(message) },
                    usesOperationalDensity: true
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)

                Divider()
                    .padding(.horizontal, 12)

                ChatComposerView(
                    text: $store.composerText,
                    placeholder: model.selectedChannel == nil ? "Select a channel" : "Message #\(model.selectedChannel?.name ?? "channel")",
                    isSending: store.isSendingMessage,
                    canSend: model.selectedChannel != nil,
                    replyingToMessage: store.replyingToMessage,
                    onCancelReply: { store.setReplyingTo(nil) },
                    usesOperationalChrome: true
                ) {
                    Task { await store.sendComposerMessage() }
                }
                .padding(12)
            }
        }
    }

    private func conversationPane(compactStyle: Bool) -> AnyView {
        switch store.surfaceState {
        case .loading:
            return AnyView(
                ChatLoadingStateView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            )
        case .empty(let title, let message):
            return AnyView(
                ChatEmptyStateView(title: title, message: message)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            )
        case .error(let title, let message):
            return AnyView(
                ChatErrorStateView(title: title, message: message) {
                    Task { await model.reloadSelectedWaddleStructure() }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            )
        case .idle:
            return AnyView(
                VStack(spacing: 0) {
                    ChatTimelineView(
                        messages: store.messages,
                        historyState: store.roomHistoryState,
                        onLoadOlderMessages: store.roomHistoryState.canLoadOlderMessages ? {
                            Task { await store.loadOlderMessages() }
                        } : nil,
                        onReply: { message in store.setReplyingTo(message) },
                        usesCompactConversationStyle: compactStyle
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                    Divider()

                    ChatComposerView(
                        text: $store.composerText,
                        placeholder: model.selectedChannel == nil ? "Select a channel" : "Message #\(model.selectedChannel?.name ?? "channel")",
                        isSending: store.isSendingMessage,
                        canSend: model.selectedChannel != nil,
                        channelName: model.selectedChannel?.name,
                        replyingToMessage: store.replyingToMessage,
                        onCancelReply: { store.setReplyingTo(nil) },
                        usesCompactConversationChrome: compactStyle
                    ) {
                        Task { await store.sendComposerMessage() }
                    }
                }
            )
        }
    }

    private var desktopMemberPane: some View {
        let activeMembers = model.chatMembers.filter(isInteractivePresence)
        let quietMembers = model.chatMembers.filter { !isInteractivePresence($0) }

        return VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text("Members")
                        .font(.headline)
                    Spacer(minLength: 12)
                    Text("\(model.chatMembers.count)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(.quaternary.opacity(0.7), in: Capsule())
                }

                HStack(spacing: 8) {
                    desktopSidebarStat(title: "Active", value: activeMembers.count)
                    desktopSidebarStat(title: "Quiet", value: quietMembers.count)
                }
            }
            .padding(18)

            Divider()
                .padding(.horizontal, 12)

            if model.chatMembers.isEmpty {
                ChatEmptyStateView(
                    title: "No members loaded",
                    message: "Channel participants will appear here.",
                    systemImage: "person.2"
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        if !activeMembers.isEmpty {
                            desktopMemberSection(title: "Active now", members: activeMembers)
                        }

                        if !quietMembers.isEmpty {
                            desktopMemberSection(title: activeMembers.isEmpty ? "Members" : "Quiet", members: quietMembers)
                        }
                    }
                    .padding(18)
                }
            }
        }
        .background(desktopPaneBackground)
    }

    private var memberPane: some View {
        ScrollView {
            ChatMemberListSection(members: model.chatMembers)
                .padding(16)
        }
        .background(workspaceBackground)
    }

    private func desktopMemberSection(title: String, members: [ChatRoomMember]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)

            VStack(spacing: 6) {
                ForEach(members) { member in
                    ChatDesktopMemberRowView(member: member)
                }
            }
        }
    }

    private func desktopSidebarStat(title: String, value: Int) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("\(value)")
                .font(.subheadline.weight(.semibold))
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private var workspaceBackground: Color {
#if os(iOS)
        return Color(.systemGroupedBackground)
#else
        return Color(nsColor: .windowBackgroundColor)
#endif
    }

    private var compactChromeBackground: Color {
#if os(iOS)
        return Color(.secondarySystemGroupedBackground)
#else
        return workspaceBackground
#endif
    }

    private var compactToolbarFill: Color {
#if os(iOS)
        return Color(.tertiarySystemGroupedBackground)
#else
        return Color.secondary.opacity(0.08)
#endif
    }

    private var desktopPaneBackground: some View {
        RoundedRectangle(cornerRadius: 26, style: .continuous)
            .fill(.thinMaterial)
            .overlay {
                RoundedRectangle(cornerRadius: 26, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.08))
            }
            .shadow(color: .black.opacity(0.06), radius: 18, y: 10)
    }

    private func channelRailBackground(isSelected: Bool) -> some ShapeStyle {
        if isSelected {
            return AnyShapeStyle(Color.accentColor.opacity(0.16))
        }
        return AnyShapeStyle(Color.secondary.opacity(0.08))
    }

    private func compactRailFill(isSelected: Bool) -> Color {
        isSelected ? Color.accentColor.opacity(0.14) : Color.secondary.opacity(0.10)
    }

    private func compactRailStroke(isSelected: Bool) -> Color {
        isSelected ? Color.accentColor.opacity(0.18) : Color.secondary.opacity(0.12)
    }

    private func compactMetaPill(text: String, systemImage: String, tint: Color = .secondary) -> some View {
        Label(text, systemImage: systemImage)
            .font(.caption.weight(.medium))
            .foregroundStyle(tint)
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .background(tint.opacity(tint == .secondary ? 0.10 : 0.12), in: Capsule())
    }

    private func scrollToSelectedChannel(using proxy: ScrollViewProxy) {
        guard let selectedChannelID = model.selectedChannelID else { return }
        withAnimation(.easeOut(duration: 0.2)) {
            proxy.scrollTo(selectedChannelID, anchor: .center)
        }
    }

    private func isInteractivePresence(_ member: ChatRoomMember) -> Bool {
        switch member.presence {
        case .available, .away, .dnd:
            return true
        case .offline, .unknown:
            return false
        }
    }
}

private struct ChatDesktopChannelRowView: View {
    let room: ChatRoomSelection
    let isSelected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(isSelected ? Color.accentColor.opacity(0.18) : Color.secondary.opacity(0.10))
                    .frame(width: 36, height: 36)
                    .overlay {
                        Image(systemName: room.subtitle == nil ? "number" : "bubble.left.and.text.bubble.right")
                            .font(.subheadline.weight(.semibold))
                            .foregroundColor(isSelected ? Color.accentColor : Color.secondary)
                    }

                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Text(room.title)
                            .font(.subheadline.weight(.semibold))
                            .lineLimit(1)

                        if room.unreadCount > 0 {
                            Text("\(room.unreadCount)")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(Color.accentColor)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 3)
                                .background(Color.accentColor.opacity(0.14), in: Capsule())
                        }
                    }

                    if let subtitle = room.subtitle, !subtitle.isEmpty {
                        Text(subtitle)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }

                    Text(lastActivityLabel)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }

                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(rowBackground)
        }
        .buttonStyle(.plain)
    }

    private var lastActivityLabel: String {
        guard let lastActivityAt = room.lastActivityAt else {
            return "No recent activity"
        }
        return "Updated \(RelativeDateTimeFormatter().localizedString(for: lastActivityAt, relativeTo: Date()))"
    }

    private var rowBackground: some View {
        RoundedRectangle(cornerRadius: 18, style: .continuous)
            .fill(isSelected ? Color.accentColor.opacity(0.12) : Color.clear)
            .overlay {
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .strokeBorder(isSelected ? Color.accentColor.opacity(0.22) : Color.primary.opacity(0.04))
            }
    }
}

private struct ChatDesktopMemberRowView: View {
    let member: ChatRoomMember

    var body: some View {
        HStack(spacing: 10) {
            Text(member.avatarInitials ?? initials(from: member.displayName))
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .frame(width: 30, height: 30)
                .background(presenceColor.opacity(0.16), in: Circle())
                .overlay(alignment: .bottomTrailing) {
                    Circle()
                        .fill(presenceColor)
                        .frame(width: 9, height: 9)
                }

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(member.displayName)
                        .font(.subheadline.weight(.medium))
                        .lineLimit(1)
                    if member.isSelf {
                        Text("you")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(.quaternary.opacity(0.6), in: Capsule())
                    }
                }

                HStack(spacing: 6) {
                    Text(member.presence.label)
                    if let role = member.role, !role.isEmpty {
                        Text("•")
                        Text(role)
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(Color.primary.opacity(0.035))
        )
    }

    private var presenceColor: Color {
        switch member.presence {
        case .available:
            return .green
        case .away:
            return .orange
        case .dnd:
            return .red
        case .offline, .unknown:
            return .secondary
        }
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }
}
