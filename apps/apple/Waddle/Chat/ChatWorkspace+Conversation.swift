import SwiftUI

// MARK: - Conversation Panes & Composers

extension WaddleChatSpaceView {
    func desktopConversationPane(showMembersInline: Bool) -> some View {
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
                Task { await model.reloadSelectedSpaceStructure() }
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

    func conversationPane(compactStyle: Bool) -> AnyView {
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
                    Task { await model.reloadSelectedSpaceStructure() }
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
}
