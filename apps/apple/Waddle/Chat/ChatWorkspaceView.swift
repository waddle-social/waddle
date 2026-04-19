import SwiftUI

struct WaddleChatWorkspaceView: View {
    @ObservedObject var model: AppModel
    @ObservedObject var store: ChatSurfaceStore
    let waddle: WaddleSummary
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

    private var isForumChannel: Bool {
        model.selectedChannel?.channelType == "forum"
    }

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
                    .glassEffect(in: .rect)
                    .transition(.move(edge: .leading))
            }
        }
        .animation(.easeOut(duration: 0.2), value: showChannelSidebar)
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
        HStack(spacing: 8) {
            Button { showChannelSidebar.toggle() } label: {
                Image(systemName: "line.3.horizontal")
                    .font(.body.weight(.medium))
                    .frame(width: 36, height: 36)
            }
            .glassEffect(.regular.interactive(), in: .circle)

            HStack(spacing: 5) {
                Image(systemName: "number")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(WaddleTheme.textMuted)
                VStack(alignment: .leading, spacing: 1) {
                    Text(store.selectedRoom?.title ?? "Select channel")
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                    Text("\(model.chatMembers.count) members")
                        .font(.caption2)
                        .foregroundStyle(WaddleTheme.textMuted)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .glassEffect(in: .capsule)

            Spacer()

            Button { showMembersSheet = true } label: {
                Image(systemName: "person.2")
                    .font(.body)
                    .frame(width: 36, height: 36)
            }
            .glassEffect(.regular.interactive(), in: .circle)
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

                    Button {
                        editWaddleName = waddle.name
                        editWaddleDescription = waddle.description ?? ""
                        showWaddleSettingsSheet = true
                    } label: {
                        Image(systemName: "gearshape")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
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

                    Button {
                        showCreateChannelSheet = true
                    } label: {
                        Image(systemName: "plus.circle.fill")
                            .font(.body)
                            .foregroundStyle(Color.accentColor)
                    }
                    .buttonStyle(.plain)

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
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 12)
                }
            }

            if !store.dmConversations.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Direct Messages")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .textCase(.uppercase)
                        .padding(.horizontal, 18)

                    ForEach(store.dmConversations) { convo in
                        Button {
                            Task { await model.openDm(peerJID: convo.peerJID, peerUsername: convo.peerUsername) }
                        } label: {
                            HStack(spacing: 8) {
                                Circle()
                                    .fill(convo.presenceShow == .available ? .green : .secondary)
                                    .frame(width: 8, height: 8)
                                Text(convo.peerUsername)
                                    .font(.subheadline)
                                    .lineLimit(1)
                                Spacer()
                                if convo.unreadCount > 0 {
                                    Text("\(convo.unreadCount)")
                                        .font(.caption2.weight(.bold))
                                        .foregroundStyle(Color.accentColor)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 2)
                                        .background(Color.accentColor.opacity(0.14), in: Capsule())
                                }
                            }
                            .padding(.horizontal, 18)
                            .padding(.vertical, 6)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.top, 8)
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
        if let dmPeer = store.activeDmPeerJID,
           let convo = store.dmConversations.first(where: { $0.peerJID == dmPeer }) {
            ChatDmConversationView(
                peerUsername: convo.peerUsername,
                messages: store.dmMessages,
                composerText: $store.dmComposerText,
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
            ChatTimelineView(
                messages: store.messages,
                historyState: store.roomHistoryState,
                onLoadOlderMessages: store.roomHistoryState.canLoadOlderMessages ? {
                    Task { await store.loadOlderMessages() }
                } : nil,
                onReply: { message in store.setReplyingTo(message) },
                onRetract: { message in Task { await model.retractMessage(message) } },
                usesOperationalDensity: true
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            ChatTypingIndicatorView(typingUsers: store.typingUsers)

            Divider()
                .padding(.horizontal, 12)

            ChatComposerView(
                text: $store.composerText,
                placeholder: model.selectedChannel == nil ? "Select a channel" : "Message #\(model.selectedChannel?.name ?? "channel")",
                isSending: store.isSendingMessage,
                canSend: model.selectedChannel != nil,
                replyingToMessage: store.replyingToMessage,
                onCancelReply: { store.setReplyingTo(nil) },
                onFileSelected: { data, name, type in
                    Task { await model.uploadAndSendFile(data: data, fileName: name, mediaType: type) }
                },
                onGifSelected: { url in
                    store.composerText = url
                    Task { await store.sendComposerMessage() }
                },
                isUploadingFile: model.isUploadingFile,
                mentionSuggestions: mentionSuggestions,
                onMentionQueryChanged: { query in store.mentionQuery = query }
                ) {
                    Task { await store.sendComposerMessage() }
                }
                .onChange(of: store.composerText) { _, _ in model.notifyComposing() }
                .padding(12)
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
                        onRetract: { message in Task { await model.retractMessage(message) } },
                        usesCompactConversationStyle: compactStyle
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                    ChatTypingIndicatorView(typingUsers: store.typingUsers)

                    Divider()

                    ChatComposerView(
                        text: $store.composerText,
                        placeholder: model.selectedChannel == nil ? "Select a channel" : "Message #\(model.selectedChannel?.name ?? "channel")",
                        isSending: store.isSendingMessage,
                        canSend: model.selectedChannel != nil,
                        channelName: model.selectedChannel?.name,
                        replyingToMessage: store.replyingToMessage,
                        onCancelReply: { store.setReplyingTo(nil) },
                        onFileSelected: { data, name, type in
                            Task { await model.uploadAndSendFile(data: data, fileName: name, mediaType: type) }
                        },
                        onGifSelected: { url in
                            store.composerText = url
                            Task { await store.sendComposerMessage() }
                        },
                        isUploadingFile: model.isUploadingFile,
                        mentionSuggestions: mentionSuggestions,
                        onMentionQueryChanged: { query in store.mentionQuery = query }
                    ) {
                        Task { await store.sendComposerMessage() }
                    }
                    .onChange(of: store.composerText) { _, _ in model.notifyComposing() }
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
