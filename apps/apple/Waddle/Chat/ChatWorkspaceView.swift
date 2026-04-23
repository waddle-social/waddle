import SwiftUI

struct WaddleChatWorkspaceView: View {
    @ObservedObject var model: AppModel
    @ObservedObject var store: ChatSurfaceStore
    let waddle: WaddleSummary
    @Environment(\.colorScheme) private var colorScheme
    @State private var showMembersSheet = false
    @State private var showChannelSidebar = false
    private var mentionSuggestions: [ChatRoomMember] {
        guard let query = store.mentionQuery else { return [] }
        if query.isEmpty { return model.chatMembers.filter { !$0.isSelf } }
        return model.chatMembers.filter { !$0.isSelf && $0.displayName.localizedCaseInsensitiveContains(query) }
    }

    @State private var showCreateChannelSheet = false
    @State private var newChannelName = ""
    @State private var newChannelDescription = ""
    @State private var newChannelType = "text"
    @State private var showNewDmSheet = false
    @State private var showCreateTopicSheet = false
    @State private var showEditChannelSheet = false
    @State private var editChannelName = ""
    @State private var editChannelDescription = ""
    @State private var showWaddleSettingsSheet = false
    @State private var editWaddleName = ""
    @State private var editWaddleDescription = ""
    @State private var showDeleteWaddleConfirm = false
    @State private var newTopicTitle = ""
    @State private var newTopicBody = ""
    @State private var forumReplyText = ""
    @State private var showDesktopChannelRail = true
    @State private var showDesktopMemberPane = false
    @AppStorage(AppConfig.scrollDirectionKey) private var scrollDirectionRaw = ChatScrollDirection.chat.rawValue

    private var isForumChannel: Bool {
        model.selectedChannel?.channelType == "forum"
    }

    private var isSocialMode: Bool {
        ChatScrollDirection(rawValue: scrollDirectionRaw) == .social
    }

    private var serverLabel: String {
        guard let host = AppConfig.normalizedServerURL(from: model.serverURLText)?.host, !host.isEmpty else {
            return model.serverURLText.replacingOccurrences(of: "https://", with: "")
        }

        return host
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
            .background(workspaceBackground.ignoresSafeArea())
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
        .sheet(isPresented: $showWaddleSettingsSheet) {
            NavigationStack {
                Form {
                    Section("Waddle Details") {
                        TextField("Name", text: $editWaddleName)
                        TextField("Description", text: $editWaddleDescription)
                    }
                    Section {
                        Button(role: .destructive) {
                            showDeleteWaddleConfirm = true
                        } label: {
                            Label("Delete Waddle", systemImage: "trash")
                        }
                    }
                }
                .navigationTitle("Waddle Settings")
#if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
#endif
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { showWaddleSettingsSheet = false }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") {
                            Task {
                                await model.updateWaddle(
                                    name: editWaddleName,
                                    description: editWaddleDescription.isEmpty ? nil : editWaddleDescription
                                )
                                showWaddleSettingsSheet = false
                            }
                        }
                        .disabled(editWaddleName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
                .alert("Delete Waddle?", isPresented: $showDeleteWaddleConfirm) {
                    Button("Delete", role: .destructive) {
                        Task {
                            await model.deleteWaddle()
                            showWaddleSettingsSheet = false
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
                        Button("Cancel") { showEditChannelSheet = false }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") {
                            guard let channelID = model.selectedChannelID else { return }
                            let idx = model.channels.firstIndex(where: { $0.id == channelID })
                            Task {
                                await model.updateChannel(
                                    channelID: channelID,
                                    name: editChannelName,
                                    description: editChannelDescription.isEmpty ? nil : editChannelDescription,
                                    position: idx ?? 0
                                )
                                showEditChannelSheet = false
                            }
                        }
                        .disabled(editChannelName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
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

    private var compactSidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
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

            WaddleTheme.divider.frame(height: 1)

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

    private var desktopChannelRail: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: 12) {
                WaddleBrandMark(size: 32)
                    .frame(width: 38, height: 38)
                    .background(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .fill(WaddleTheme.surfaceRaised)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                    }

                VStack(alignment: .leading, spacing: 3) {
                    Text(waddle.name)
                        .font(.system(size: 17, weight: .semibold))
                        .lineLimit(1)
                    Text(serverLabel)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(WaddleTheme.textSecondary)
                        .lineLimit(1)
                }

                Spacer(minLength: 0)

                DesktopActionButton(systemName: "plus", accessibilityLabel: "Create channel") {
                    showCreateChannelSheet = true
                }

                DesktopActionButton(systemName: "gearshape", accessibilityLabel: "Workspace settings") {
                    editWaddleName = waddle.name
                    editWaddleDescription = waddle.description ?? ""
                    showWaddleSettingsSheet = true
                }

                DesktopActionButton(systemName: "sidebar.leading", accessibilityLabel: "Collapse channels") {
                    withAnimation(.easeOut(duration: 0.18)) {
                        showDesktopChannelRail = false
                    }
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 16)

            Divider()
                .overlay(WaddleTheme.divider)

            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .center, spacing: 10) {
                    Text("Channels")
                        .font(.system(size: 12, weight: .bold))
                        .tracking(1.4)
                        .foregroundStyle(WaddleTheme.textMuted)
                        .textCase(.uppercase)

                    Spacer()

                    if model.isJoined(waddle.id) {
                        Text("Joined")
                            .font(.system(size: 10, weight: .bold))
                            .foregroundStyle(WaddleTheme.presenceOnline)
                            .padding(.horizontal, 7)
                            .padding(.vertical, 4)
                            .background(WaddleTheme.presenceOnline.opacity(0.10), in: Capsule())
                    } else {
                        joinButton
                    }
                }

                Text("\(store.rooms.count) channels · \(model.members.count) people")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(WaddleTheme.textSecondary)
                    .lineLimit(1)
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)

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
                    LazyVStack(alignment: .leading, spacing: 4) {
                        ForEach(store.rooms) { room in
                            ChatDesktopChannelRowView(
                                room: room,
                                isSelected: model.selectedChannelID == room.id
                            ) {
                                Task { await model.selectChannel(room.id) }
                            }
                            .contextMenu {
                                Button {
                                    editChannelName = room.title
                                    editChannelDescription = room.subtitle ?? ""
                                    Task { await model.selectChannel(room.id) }
                                    showEditChannelSheet = true
                                } label: {
                                    Label("Edit Channel", systemImage: "pencil")
                                }
                            }
                        }

                        if !store.dmConversations.isEmpty {
                            Text("Direct messages")
                                .font(.system(size: 11, weight: .semibold))
                                .foregroundStyle(WaddleTheme.textMuted)
                                .textCase(.uppercase)
                                .padding(.horizontal, 12)
                                .padding(.top, 18)
                                .padding(.bottom, 6)

                            ForEach(store.dmConversations) { convo in
                                Button {
                                    Task { await model.openDm(peerJID: convo.peerJID, peerUsername: convo.peerUsername) }
                                } label: {
                                    HStack(spacing: 10) {
                                        Circle()
                                            .fill(convo.presenceShow == .available ? WaddleTheme.presenceOnline : WaddleTheme.presenceOffline)
                                            .frame(width: 8, height: 8)
                                        Text(convo.peerUsername)
                                            .font(.system(size: 14, weight: .medium))
                                            .foregroundStyle(WaddleTheme.textPrimary)
                                            .lineLimit(1)
                                        Spacer()
                                        if convo.unreadCount > 0 {
                                            Text("\(convo.unreadCount)")
                                                .font(.caption2.weight(.bold))
                                                .foregroundStyle(Color.white)
                                                .padding(.horizontal, 6)
                                                .padding(.vertical, 2)
                                                .background(WaddleTheme.unreadBadge, in: Capsule())
                                        }
                                    }
                                    .padding(.horizontal, 12)
                                    .padding(.vertical, 8)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                }
            }

            Divider()
                .overlay(WaddleTheme.divider)

            Button {
                if showDesktopMemberPane {
                    withAnimation(.easeOut(duration: 0.18)) {
                        showDesktopMemberPane = false
                    }
                } else if model.chatMembers.isEmpty {
                    showMembersSheet = true
                } else {
                    withAnimation(.easeOut(duration: 0.18)) {
                        showDesktopMemberPane = true
                    }
                }
            } label: {
                HStack(spacing: 12) {
                    Image(systemName: "person.2")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(WaddleTheme.textSecondary)
                    Text("Members")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(WaddleTheme.textPrimary)
                    Spacer()
                    Text("\(model.chatMembers.count)")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(WaddleTheme.textSecondary)
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 16)
            }
            .buttonStyle(.plain)
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(WaddleTheme.sidebarBackground)
    }

    private func desktopConversationPane(showMembersInline: Bool) -> some View {
        VStack(spacing: 0) {
            ChatConversationHeaderView(
                room: store.selectedRoom,
                bannerState: store.bannerState,
                memberCount: model.chatMembers.count,
                messageCount: store.messages.count,
                showsMemberButton: !showMembersInline || !showDesktopMemberPane,
                onShowMembers: {
                    if showMembersInline {
                        withAnimation(.easeOut(duration: 0.18)) {
                            showDesktopMemberPane = true
                        }
                    } else {
                        showMembersSheet = true
                    }
                },
                usesOperationalChrome: true
            )

            desktopConversationContent
        }
        .background(WaddleTheme.chatBackground)
    }

    @ViewBuilder
    private var desktopConversationContent: some View {
        if let dmPeer = store.activeDmPeerJID,
           let convo = store.dmConversations.first(where: { $0.peerJID == dmPeer }) {
            ChatDmConversationView(
                peerUsername: convo.peerUsername,
                messages: store.dmMessages,
                composerText: $store.dmComposerText,
                isUploadingFile: model.isUploadingFile,
                onFileSelected: { data, name, type in
                    Task {
                        await model.uploadAndSendDmFile(
                            data: data,
                            fileName: name,
                            mediaType: type,
                            peerJID: convo.peerJID
                        )
                    }
                },
                avatarDataBySenderID: { model.avatarData(forSenderID: $0) },
                onRequestAvatar: { model.requestAvatarIfNeeded(forSenderID: $0) },
                onSend: {
                    Task { await model.sendDm(body: store.dmComposerText); store.dmComposerText = "" }
                },
                onBack: { model.closeDm() }
            )
        } else {
            channelConversationContent
        }
    }

    @ViewBuilder
    private var channelConversationContent: some View {
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
            if isForumChannel {
                forumContent
            } else {
                regularChatContent
            }
        }
    }

    @ViewBuilder
    private var forumContent: some View {
        if let threadID = model.selectedForumThreadID,
           let topic = model.forumTopics.first(where: { $0.id == threadID }) {
            ChatForumThreadView(
                topic: topic,
                replies: model.threadReplies(for: threadID),
                replyText: $forumReplyText,
                onSendReply: {
                    let text = forumReplyText.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { return }
                    Task {
                        await model.sendForumReply(body: text, threadID: threadID)
                        forumReplyText = ""
                    }
                },
                onBack: { model.selectedForumThreadID = nil }
            )
        } else {
            VStack(spacing: 0) {
                HStack {
                    Text("Topics")
                        .font(.headline)
                    Spacer()
                    Button {
                        showCreateTopicSheet = true
                    } label: {
                        Label("New Topic", systemImage: "plus.circle.fill")
                            .font(.subheadline.weight(.medium))
                    }
                }
                .padding(12)

                Divider()

                ChatForumTopicListView(
                    topics: model.forumTopics,
                    onSelectTopic: { topic in model.selectedForumThreadID = topic.id }
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .sheet(isPresented: $showCreateTopicSheet) {
                NavigationStack {
                    Form {
                        TextField("Topic title", text: $newTopicTitle)
                        TextField("First message", text: $newTopicBody, axis: .vertical)
                            .lineLimit(3...8)
                    }
                    .navigationTitle("New Topic")
#if os(iOS)
                    .navigationBarTitleDisplayMode(.inline)
#endif
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Cancel") { showCreateTopicSheet = false }
                        }
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Create") {
                                let title = newTopicTitle.trimmingCharacters(in: .whitespacesAndNewlines)
                                let body = newTopicBody.trimmingCharacters(in: .whitespacesAndNewlines)
                                guard !title.isEmpty else { return }
                                Task {
                                    await model.sendForumTopic(title: title, body: body.isEmpty ? title : body)
                                    newTopicTitle = ""
                                    newTopicBody = ""
                                    showCreateTopicSheet = false
                                }
                            }
                            .disabled(newTopicTitle.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        }
                    }
                }
                .presentationDetents([.medium])
            }
        }
    }

    @ViewBuilder
    private var regularChatContent: some View {
        VStack(spacing: 0) {
            if isSocialMode {
                regularComposer
                ChatTypingIndicatorView(typingUsers: store.typingUsers)
                Divider()
                    .padding(.horizontal, 12)
            }

            ChatTimelineView(
                messages: store.mainTimelineMessages,
                historyState: store.roomHistoryState,
                onLoadOlderMessages: store.roomHistoryState.canLoadOlderMessages ? {
                    Task { await store.loadOlderMessages() }
                } : nil,
                onReply: { message in store.setReplyingTo(message) },
                onRetract: { message in Task { await model.retractMessage(message) } },
                onOpenThread: { message in store.openThreadPanel(forRootID: message.id) },
                childrenByThreadID: store.childrenByThreadID,
                firstUnreadMessageID: store.firstUnreadMessageID,
                avatarDataBySenderID: { model.avatarData(forSenderID: $0) },
                onRequestAvatar: { model.requestAvatarIfNeeded(forSenderID: $0) },
                usesOperationalDensity: true
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            if !isSocialMode {
                ChatTypingIndicatorView(typingUsers: store.typingUsers)
                Divider()
                    .padding(.horizontal, 12)
                regularComposer
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
                    if isSocialMode {
                        compactComposer
                        ChatTypingIndicatorView(typingUsers: store.typingUsers)
                        Divider()
                    }

                    ChatTimelineView(
                        messages: store.mainTimelineMessages,
                        historyState: store.roomHistoryState,
                        onLoadOlderMessages: store.roomHistoryState.canLoadOlderMessages ? {
                            Task { await store.loadOlderMessages() }
                        } : nil,
                        onReply: { message in store.setReplyingTo(message) },
                        onRetract: { message in Task { await model.retractMessage(message) } },
                        onOpenThread: { message in store.openThreadPanel(forRootID: message.id) },
                        childrenByThreadID: store.childrenByThreadID,
                        firstUnreadMessageID: store.firstUnreadMessageID,
                        avatarDataBySenderID: { model.avatarData(forSenderID: $0) },
                        onRequestAvatar: { model.requestAvatarIfNeeded(forSenderID: $0) },
                        usesCompactConversationStyle: compactStyle
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                    if !isSocialMode {
                        ChatTypingIndicatorView(typingUsers: store.typingUsers)
                        Divider()
                        compactComposer
                    }
                }
            )
        }
    }

    private var regularComposer: some View {
        ChatComposerView(
            text: $store.composerText,
            placeholder: model.selectedChannel == nil ? "Select a channel" : "Message #\(model.selectedChannel?.name ?? "channel")",
            isSending: store.isSendingMessage,
            canSend: model.selectedChannel != nil,
            replyingToMessage: store.replyingToMessage,
            onCancelReply: { store.setReplyingTo(nil) },
            onFileSelected: { data, name, type in
                Task {
                    await model.uploadAndSendFile(
                        data: data,
                        fileName: name,
                        mediaType: type,
                        replyTo: store.replyingToMessage
                    )
                }
            },
            onGifSelected: { url in
                store.composerText = url
                Task { await store.sendComposerMessage() }
            },
            isUploadingFile: model.isUploadingFile,
            mentionSuggestions: mentionSuggestions,
            onMentionQueryChanged: { query in store.mentionQuery = query },
            usesOperationalChrome: true
        ) {
            Task { await store.sendComposerMessage() }
        }
        .onChange(of: store.composerText) { _, _ in model.notifyComposing() }
        .padding(12)
    }

    private var compactComposer: some View {
        ChatComposerView(
            text: $store.composerText,
            placeholder: model.selectedChannel == nil ? "Select a channel" : "Message #\(model.selectedChannel?.name ?? "channel")",
            isSending: store.isSendingMessage,
            canSend: model.selectedChannel != nil,
            channelName: model.selectedChannel?.name,
            replyingToMessage: store.replyingToMessage,
            onCancelReply: { store.setReplyingTo(nil) },
            onFileSelected: { data, name, type in
                Task {
                    await model.uploadAndSendFile(
                        data: data,
                        fileName: name,
                        mediaType: type,
                        replyTo: store.replyingToMessage
                    )
                }
            },
            onGifSelected: { url in
                store.composerText = url
                Task { await store.sendComposerMessage() }
            },
            isUploadingFile: model.isUploadingFile,
            mentionSuggestions: mentionSuggestions,
            onMentionQueryChanged: { query in store.mentionQuery = query },
            usesCompactConversationChrome: true
        ) {
            Task { await store.sendComposerMessage() }
        }
        .onChange(of: store.composerText) { _, _ in model.notifyComposing() }
    }

    private var desktopMemberPane: some View {
        let activeMembers = model.chatMembers.filter(isInteractivePresence)
        let quietMembers = model.chatMembers.filter { !isInteractivePresence($0) }

        return VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text("Members")
                        .font(.system(size: 17, weight: .semibold))
                    Button {
                        withAnimation(.easeOut(duration: 0.18)) {
                            showDesktopMemberPane = false
                        }
                    } label: {
                        Image(systemName: "sidebar.trailing")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    Spacer(minLength: 12)
                    Text("\(model.chatMembers.count)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(WaddleTheme.textSecondary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(WaddleTheme.surfaceRaised, in: Capsule())
                        .overlay {
                            Capsule()
                                .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                        }
                }

                HStack(spacing: 8) {
                    desktopSidebarStat(title: "Active", value: activeMembers.count)
                    desktopSidebarStat(title: "Quiet", value: quietMembers.count)
                }
            }
            .padding(18)

            Divider()
                .overlay(WaddleTheme.divider)

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
        .background(WaddleTheme.sidebarBackground)
    }

    private var desktopCollapsedChannelRail: some View {
        VStack(spacing: 12) {
            Button {
                withAnimation(.easeOut(duration: 0.18)) {
                    showDesktopChannelRail = true
                }
            } label: {
                Image(systemName: "sidebar.left")
                    .font(.caption.weight(.semibold))
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.plain)

            Text("\(store.rooms.count)")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .frame(width: 28, height: 18)
                .background(WaddleTheme.surfaceRaised, in: Capsule())
                .overlay {
                    Capsule()
                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                }

            Spacer(minLength: 0)
        }
        .padding(.vertical, 12)
        .frame(maxHeight: .infinity)
        .background(WaddleTheme.sidebarBackground)
    }

    private var desktopCollapsedMemberRail: some View {
        VStack(spacing: 12) {
            Button {
                withAnimation(.easeOut(duration: 0.18)) {
                    showDesktopMemberPane = true
                }
            } label: {
                Image(systemName: "person.2")
                    .font(.caption.weight(.semibold))
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.plain)

            Text("\(model.chatMembers.count)")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .frame(width: 28, height: 18)
                .background(WaddleTheme.surfaceRaised, in: Capsule())
                .overlay {
                    Capsule()
                        .strokeBorder(WaddleTheme.divider, lineWidth: 1)
                }

            Spacer(minLength: 0)
        }
        .padding(.vertical, 12)
        .frame(maxHeight: .infinity)
        .background(WaddleTheme.sidebarBackground)
    }

    private var memberPane: some View {
        ScrollView {
            ChatMemberListSection(members: model.chatMembers)
                .padding(16)
        }
        .background(WaddleTheme.sidebarBackground)
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
                        .contextMenu {
                            if !member.isSelf {
                                Button {
                                    let peerJID = "\(member.displayName.lowercased())@\(jidDomain(model.session?.jid ?? ""))"
                                    Task { await model.openDm(peerJID: peerJID, peerUsername: member.displayName) }
                                } label: {
                                    Label("Message", systemImage: "bubble.left")
                                }

                                Menu("Change Role") {
                                    ForEach(["member", "moderator", "admin"], id: \.self) { role in
                                        Button(role.capitalized) {
                                            Task { await model.changeMemberRole(userID: member.id, role: role) }
                                        }
                                    }
                                }

                                Divider()

                                Button(role: .destructive) {
                                    Task { await model.removeMember(userID: member.id) }
                                } label: {
                                    Label("Remove", systemImage: "person.badge.minus")
                                }
                            }
                        }
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
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(WaddleTheme.divider, lineWidth: 1)
        }
    }

    private var workspaceBackground: Color {
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

    private func isInteractivePresence(_ member: ChatRoomMember) -> Bool {
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

private struct ChatDesktopChannelRowView: View {
    let room: ChatRoomSelection
    let isSelected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(alignment: .center, spacing: 10) {
                Image(systemName: room.subtitle == nil ? "number" : "bubble.left.and.text.bubble.right")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(isSelected ? WaddleTheme.accent : WaddleTheme.textMuted)
                    .frame(width: 16)

                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 8) {
                        Text(room.title)
                            .font(.system(size: 14, weight: isSelected ? .semibold : .medium))
                            .foregroundStyle(WaddleTheme.textPrimary)
                            .lineLimit(1)

                        if room.unreadCount > 0 {
                            Text("\(room.unreadCount)")
                                .font(.caption2.weight(.bold))
                                .foregroundStyle(Color.white)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(WaddleTheme.unreadBadge, in: Capsule())
                        }
                    }

                    if let subtitle = room.subtitle, !subtitle.isEmpty {
                        Text(subtitle)
                            .font(.caption)
                            .foregroundStyle(WaddleTheme.textSecondary)
                            .lineLimit(1)
                    }
                }

                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(rowBackground)
        }
        .buttonStyle(.plain)
    }

    private var rowBackground: some View {
        RoundedRectangle(cornerRadius: 14, style: .continuous)
            .fill(isSelected ? WaddleTheme.channelSelected : Color.clear)
            .overlay {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(isSelected ? Color.accentColor.opacity(0.20) : Color.clear)
            }
    }
}

private struct ChatDesktopMemberRowView: View {
    let member: ChatRoomMember

    var body: some View {
        HStack(spacing: 10) {
            Text(member.avatarInitials ?? initials(from: member.displayName))
                .font(.caption.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
                .frame(width: 30, height: 30)
                .background(presenceColor.opacity(0.16), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
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
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(WaddleTheme.surfaceRaised)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(WaddleTheme.divider, lineWidth: 1)
        }
    }

    private var presenceColor: Color {
        switch member.presence {
        case .available: return WaddleTheme.presenceOnline
        case .away:      return WaddleTheme.presenceAway
        case .dnd:       return WaddleTheme.presenceDnd
        case .offline, .unknown: return WaddleTheme.presenceOffline
        }
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }
}
