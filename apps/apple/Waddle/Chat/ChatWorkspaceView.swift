import SwiftUI

struct SidebarSpaceGroup: Identifiable {
    let space: SpaceSummary
    let rooms: [ChatRoomSelection]

    var id: String { space.id }
}

struct WaddleChatSpaceView: View {
    @ObservedObject var model: AppModel
    @ObservedObject var store: ChatSurfaceStore
    let space: SpaceSummary
    @Environment(\.colorScheme) private var colorScheme
    @State var showMembersSheet = false
    @State var showChannelSidebar = false
    var mentionSuggestions: [ChatRoomMember] {
        guard let query = store.mentionQuery else { return [] }
        if query.isEmpty { return model.chatMembers.filter { !$0.isSelf } }
        return model.chatMembers.filter { !$0.isSelf && $0.displayName.localizedCaseInsensitiveContains(query) }
    }

    @State var showCreateChannelSheet = false
    @State private var newChannelName = ""
    @State private var newChannelDescription = ""
    @State private var newChannelType = "text"
    @State var showNewDmSheet = false
    @State var showCreateTopicSheet = false
    @State var showEditChannelSheet = false
    @State var editingChannelID: String?
    @State var editChannelName = ""
    @State var editChannelDescription = ""
    @State var showSpaceSettingsSheet = false
    @State var editSpaceName = ""
    @State var editSpaceDescription = ""
    @State private var showDeleteSpaceConfirm = false
    @State var newTopicTitle = ""
    @State var newTopicBody = ""
    @State var forumReplyText = ""
    @State var showDesktopChannelRail = true
    @State var showDesktopMemberPane = false
    @AppStorage(AppConfig.scrollDirectionKey) private var scrollDirectionRaw = ChatScrollDirection.chat.rawValue

    var isForumChannel: Bool {
        model.selectedChannel?.channelType == "forum"
    }

    var isSocialMode: Bool {
        ChatScrollDirection(rawValue: scrollDirectionRaw) == .social
    }

    var serverLabel: String {
        guard let host = AppConfig.normalizedServerURL(from: model.serverURLText)?.host, !host.isEmpty else {
            return model.serverURLText.replacingOccurrences(of: "https://", with: "")
        }

        return host
    }

    var channelGroups: [SidebarSpaceGroup] {
        let roomsByID = Dictionary(uniqueKeysWithValues: store.rooms.map { ($0.id, $0) })
        let discoveredSpaces = model.spaces.isEmpty ? [space] : model.spaces

        return discoveredSpaces.compactMap { discoveredSpace in
            let rooms = model.channels
                .filter { ($0.spaceID ?? discoveredSpace.id) == discoveredSpace.id }
                .compactMap { roomsByID[$0.id] }
            if rooms.isEmpty && !store.rooms.isEmpty { return nil }
            return SidebarSpaceGroup(space: discoveredSpace, rooms: rooms)
        }
    }

    var body: some View {
        GeometryReader { proxy in
            let compactLayout = proxy.size.width < 760
            let showMembersInline = proxy.size.width > 1280

            Group {
                if compactLayout {
                    compactLayoutView
                } else {
                    regularLayout(showMembersInline: showMembersInline)
                }
            }
            .background(spaceBackground.ignoresSafeArea())
            .overlay(alignment: .top) {
                if let toast = store.notificationToast {
                    ChatNotificationToastView(toast: toast) {
                        withAnimation { store.notificationToast = nil }
                    }
                    .padding(.top, 8)
                    .animation(.spring(duration: 0.3), value: store.notificationToast?.id)
                }
            }
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
        .sheet(isPresented: Binding(
            get: { !store.activeThreadStack.isEmpty },
            set: { isShown in if !isShown { store.closeThreadPanel() } }
        )) {
            if let root = store.threadPanelRoot {
                ChatThreadPanelView(
                    root: root,
                    replies: store.threadPanelChildren,
                    composerText: $store.threadComposerText,
                    isSending: store.isSendingThreadMessage,
                    isUploadingFile: model.isUploadingFile,
                    canGoBack: store.canPopThreadPanel,
                    threadChildCount: { id in store.threadChildCount(forRootID: id) },
                    onOpenNestedThread: { msg in store.pushThreadPanel(forRootID: msg.id) },
                    onBack: { store.popThreadPanel() },
                    onFileSelected: { data, name, type in
                        guard let rootID = store.activeThreadParentID else { return }
                        Task {
                            await model.uploadAndSendFile(
                                data: data,
                                fileName: name,
                                mediaType: type,
                                threadRootID: rootID
                            )
                        }
                    },
                    onSend: { Task { await store.sendThreadComposerMessage() } },
                    onClose: { store.closeThreadPanel() },
                    avatarDataBySenderID: { model.avatarData(forSenderID: $0) },
                    onRequestAvatar: { model.requestAvatarIfNeeded(forSenderID: $0) }
                )
                .presentationDetents([.large])
            }
        }
        .sheet(isPresented: $showCreateChannelSheet) {
            createChannelSheet
        }
        .sheet(isPresented: $showNewDmSheet) {
            NavigationStack {
                List(model.chatMembers.filter { !$0.isSelf }) { member in
                    Button {
                        let peerJID = "\(member.displayName.lowercased())@\(jidDomain(model.session?.jid ?? ""))"
                        showNewDmSheet = false
                        Task { await model.openDm(peerJID: peerJID, peerUsername: member.displayName) }
                    } label: {
                        HStack(spacing: 10) {
                            Text(member.avatarInitials ?? String(member.displayName.prefix(2)).uppercased())
                                .font(.caption.weight(.semibold))
                                .frame(width: 32, height: 32)
                                .background(.quaternary, in: Circle())
                            Text(member.displayName)
                                .font(.subheadline)
                        }
                    }
                }
                .navigationTitle("New Message")
#if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
#endif
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { showNewDmSheet = false }
                    }
                }
            }
            .presentationDetents([.medium, .large])
        }
        .sheet(isPresented: $showSpaceSettingsSheet) {
            NavigationStack {
                Form {
                    Section("Space Details") {
                        TextField("Name", text: $editSpaceName)
                        TextField("Description", text: $editSpaceDescription)
                    }
                    Section {
                        Button(role: .destructive) {
                            showDeleteSpaceConfirm = true
                        } label: {
                            Label("Delete Space", systemImage: "trash")
                        }
                    }
                }
                .navigationTitle("Space Settings")
#if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
#endif
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { showSpaceSettingsSheet = false }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") {
                            Task {
                                await model.updateSpace(
                                    name: editSpaceName,
                                    description: editSpaceDescription.isEmpty ? nil : editSpaceDescription
                                )
                                showSpaceSettingsSheet = false
                            }
                        }
                        .disabled(editSpaceName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
                .alert("Delete Space?", isPresented: $showDeleteSpaceConfirm) {
                    Button("Delete", role: .destructive) {
                        Task {
                            await model.deleteSpace()
                            showSpaceSettingsSheet = false
                        }
                    }
                    Button("Cancel", role: .cancel) {}
                } message: {
                    Text("This action cannot be undone. All channels and messages will be permanently deleted.")
                }
            }
            .presentationDetents([.medium])
        }
        .sheet(isPresented: $showEditChannelSheet) {
            NavigationStack {
                Form {
                    TextField("Channel name", text: $editChannelName)
                    TextField("Description", text: $editChannelDescription)
                }
                .navigationTitle("Edit Channel")
#if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
#endif
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") {
                            editingChannelID = nil
                            showEditChannelSheet = false
                        }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") {
                            guard let channelID = editingChannelID else { return }
                            let channelPosition = model.channels.first(where: { $0.id == channelID })?.position ?? 0
                            Task {
                                await model.updateChannel(
                                    channelID: channelID,
                                    name: editChannelName,
                                    description: editChannelDescription.isEmpty ? nil : editChannelDescription,
                                    position: channelPosition
                                )
                                editingChannelID = nil
                                showEditChannelSheet = false
                            }
                        }
                        .disabled(editingChannelID == nil || editChannelName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .presentationDetents([.medium])
        }
    }

    private var createChannelSheet: some View {
        NavigationStack {
            Form {
                Section("Channel Details") {
                    TextField("Channel name", text: $newChannelName)
                    TextField("Description (optional)", text: $newChannelDescription)
                }

                Section("Type") {
                    Picker("Channel type", selection: $newChannelType) {
                        Text("Text").tag("text")
                        Text("Forum").tag("forum")
                    }
                    .pickerStyle(.segmented)
                }
            }
            .navigationTitle("New Channel")
#if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
#endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        showCreateChannelSheet = false
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        Task {
                            await model.createChannel(
                                name: newChannelName,
                                description: newChannelDescription.isEmpty ? nil : newChannelDescription,
                                channelType: newChannelType
                            )
                            newChannelName = ""
                            newChannelDescription = ""
                            newChannelType = "text"
                            showCreateChannelSheet = false
                        }
                    }
                    .disabled(newChannelName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.isCreatingChannel)
                }
            }
        }
        .presentationDetents([.medium])
    }

    private var compactLayoutView: some View {
        ZStack(alignment: .leading) {
            VStack(spacing: 0) {
                compactChatHeader
                if store.bannerState.isVisible {
                    ChatConnectionBannerView(state: store.bannerState, usesOperationalChrome: true)
                        .padding(.horizontal, 12)
                        .padding(.bottom, 8)
                }
                conversationPane(compactStyle: true)
            }
            .background(WaddleTheme.chatBackground)

            if showChannelSidebar {
                Color.black.opacity(0.4)
                    .ignoresSafeArea()
                    .onTapGesture { showChannelSidebar = false }

                compactSidebar
                    .frame(width: WaddleTheme.sidebarWidth)
                    .waddleGlass(in: .rect)
                    .transition(.move(edge: .leading))
            }
        }
        .animation(.easeOut(duration: 0.2), value: showChannelSidebar)
    }

    @ViewBuilder
    private func regularLayout(showMembersInline: Bool) -> some View {
#if os(macOS)
        let shellShape = RoundedRectangle(cornerRadius: 24, style: .continuous)

        HStack(spacing: 0) {
            if showDesktopChannelRail {
                desktopChannelRail
                    .frame(minWidth: 240, idealWidth: 264, maxWidth: 292)
            } else {
                desktopCollapsedChannelRail
                    .frame(width: 52)
            }

            Divider()
                .overlay(WaddleTheme.divider)

            desktopConversationPane(showMembersInline: showMembersInline)
                .frame(minWidth: 620, maxWidth: .infinity, maxHeight: .infinity)

            if showMembersInline {
                Divider()
                    .overlay(WaddleTheme.divider)

                if showDesktopMemberPane {
                    desktopMemberPane
                        .frame(width: 240)
                } else {
                    desktopCollapsedMemberRail
                        .frame(width: 52)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(WaddleTheme.chatBackground, in: shellShape)
        .overlay {
            shellShape
                .strokeBorder(WaddleTheme.divider, lineWidth: 1)
        }
        .clipShape(shellShape)
        .shadow(
            color: .black.opacity(colorScheme == .dark ? 0.24 : 0.06),
            radius: colorScheme == .dark ? 28 : 18,
            y: colorScheme == .dark ? 10 : 6
        )
#else
        VStack(spacing: 0) {
            compactChatHeader

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

    private var compactChatHeader: some View {
        let activeCount = model.chatMembers.filter(isInteractivePresence).count
        let totalCount = model.chatMembers.count
        let topic = store.selectedRoom?.subtitle

        return HStack(alignment: .center, spacing: 10) {
            Button { showChannelSidebar.toggle() } label: {
                Image(systemName: "line.3.horizontal")
                    .font(.body.weight(.medium))
                    .frame(width: 36, height: 36)
            }
            .waddleInteractiveGlass(in: .circle)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Image(systemName: "number")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(WaddleTheme.accent)
                    Text(store.selectedRoom?.title ?? "Select channel")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(WaddleTheme.textPrimary)
                        .lineLimit(1)
                }
                if let topic, !topic.isEmpty {
                    Text(topic)
                        .font(.caption2)
                        .foregroundStyle(WaddleTheme.textSecondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }

            Spacer(minLength: 4)

            Button { showMembersSheet = true } label: {
                HStack(spacing: 4) {
                    Circle()
                        .fill(WaddleTheme.presenceOnline)
                        .frame(width: 6, height: 6)
                    Text("\(activeCount)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(WaddleTheme.textPrimary)
                    Text("/\(totalCount)")
                        .font(.caption)
                        .foregroundStyle(WaddleTheme.textMuted)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
            }
            .waddleInteractiveGlass(in: .capsule)
            .accessibilityLabel("\(activeCount) of \(totalCount) members active")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var joinButton: some View {
        Label("Member", systemImage: "checkmark.circle.fill")
            .foregroundStyle(.green)
    }

    private var spaceBackground: Color {
        WaddleTheme.chatBackground
    }

    private var desktopPaneBackground: some View {
        WaddleTheme.sidebarBackground
    }

    private var desktopPaneFill: Color {
        WaddleTheme.sidebarBackground
    }

    private func channelRailBackground(isSelected: Bool) -> some ShapeStyle {
        if isSelected {
            return AnyShapeStyle(Color.accentColor.opacity(0.16))
        }
        return AnyShapeStyle(Color.secondary.opacity(0.08))
    }

    func isInteractivePresence(_ member: ChatRoomMember) -> Bool {
        switch member.presence {
        case .available, .away, .dnd:
            return true
        case .offline, .unknown:
            return false
        }
    }
}

private extension View {
    func compactChrome<S: Shape>(in shape: S) -> some View {
        background {
            shape
                .fill(.ultraThinMaterial)
                .overlay {
                    shape.stroke(Color.white.opacity(0.08), lineWidth: 1)
                }
        }
        .shadow(color: .black.opacity(0.18), radius: 16, y: 8)
    }
}
