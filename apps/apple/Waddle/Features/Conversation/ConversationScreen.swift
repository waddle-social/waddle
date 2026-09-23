import SwiftUI
import WaddleKit

/// A channel, group DM or 1:1 conversation: header, timeline, typing line
/// and composer.
struct ConversationScreen: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID

    init(conversation: ConversationID) {
        self.conversation = conversation
    }

    var body: some View {
        ConversationContent(
            conversation: conversation,
            composer: ComposerDraftStore.shared.model(for: ComposerDraftStore.Key(
                account: session.account.jid,
                conversation: conversation,
                thread: nil
            ))
        )
        .id(conversation)
    }
}

/// The screen body, created once per conversation so its action model
/// and unread anchor reset when the conversation changes.
private struct ConversationContent: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var actions: MessageActionModel
    @State private var unreadAnchorID: String?
    @State private var showsPinsSheet = false
    /// A focus request whose row is not loaded yet, while history pages in.
    @State private var pendingFocus: FocusRequest?
    @State private var showsRevealMiss = false
    private var placement = ConversationInspectorPlacement()

    let conversation: ConversationID
    let composer: ComposerModel

    init(conversation: ConversationID, composer: ComposerModel) {
        self.conversation = conversation
        self.composer = composer
        _actions = State(initialValue: MessageActionModel(conversation: conversation, composer: composer, isThread: false))
    }

    var body: some View {
        let header = ConversationHeaderText.make(for: conversation, session: session)
        ConversationTimelineContainer(conversation: conversation, header: header, unreadAnchorID: unreadAnchorID)
            .overlay(alignment: .top) {
                ZStack {
                    if let phase = revealPhase {
                        TimelineRevealBanner(phase: phase)
                            .transition(.move(edge: .top).combined(with: .opacity))
                    }
                }
                .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: revealPhase)
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                ConnectionBanner(status: session.connection)
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                    ConversationTypingIndicator(conversation: conversation)
                    ConversationComposer(
                        model: composer,
                        conversation: conversation,
                        thread: nil,
                        placeholder: header.composerPlaceholder
                    )
                }
            }
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: session.connection)
            .navigationTitle(header.title)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            #if os(macOS)
            .navigationSubtitle(ConversationHeaderText.subtitle(for: conversation, session: session) ?? "")
            #endif
            .toolbar {
                ConversationToolbar(
                    conversation: conversation,
                    onSearch: { navigation.sheet = .search(conversation) },
                    onPins: showPins,
                    onDetails: { navigation.showDetails(of: conversation, usesInspector: placement.usesInspector) }
                )
            }
            .environment(actions)
            .modifier(MessageActionDialogs(actions: actions))
            .sheet(isPresented: $showsPinsSheet) {
                PinnedMessagesSheet(conversation: conversation)
                    .environment(session)
                    .environment(navigation)
            }
            .task(id: conversation) {
                await open()
            }
            .onChange(of: navigation.focusRequest, initial: true) { _, request in
                guard let request, request.conversation == conversation else { return }
                navigation.focusRequest = nil
                focus(on: request)
            }
            .task(id: pendingFocus) {
                await reveal(pendingFocus)
            }
            .task(id: showsRevealMiss) {
                guard showsRevealMiss else { return }
                try? await Task.sleep(nanoseconds: 2_500_000_000)
                guard !Task.isCancelled else { return }
                showsRevealMiss = false
            }
            .onDisappear {
                session.close(conversation)
                session.stopTyping(in: conversation, notify: true)
            }
    }

    /// Captures the unread count before opening clears it, so the divider
    /// lands before the first message that was unread.
    private func open() async {
        let unread = session.unread.count(for: conversation)
        await session.open(conversation)
        guard unread > 0, !Task.isCancelled else { return }
        let items = session.timelines.timeline(for: conversation).feedItems
        unreadAnchorID = TimelineUnreadAnchor.firstUnreadID(in: items, unreadCount: unread)
    }

    private var revealPhase: TimelineRevealBanner.Phase? {
        if pendingFocus != nil { return .finding }
        return showsRevealMiss ? .missed : nil
    }

    /// Scrolls to a loaded row at once; otherwise hands the request to
    /// `reveal`, replacing (and so cancelling) any search still running.
    private func focus(on request: FocusRequest) {
        showsRevealMiss = false
        // Rows are keyed by their primary id; resolve any alias.
        if let item = session.timelines.timeline(for: conversation).item(withID: request.messageID) {
            pendingFocus = nil
            actions.scrollRequest = item.id
        } else {
            pendingFocus = request
        }
    }

    /// Pages history back until the requested row is loaded, then scrolls
    /// to it and highlights it.
    private func reveal(_ request: FocusRequest?) async {
        guard let request else { return }
        let outcome = await session.reveal(messageID: request.messageID, in: conversation)
        guard !Task.isCancelled else { return }
        pendingFocus = nil
        if case let .found(itemID) = outcome {
            actions.scrollRequest = itemID
        } else {
            showsRevealMiss = true
        }
    }

    private func showPins() {
        if placement.usesInspector {
            navigation.inspector = navigation.inspector == .pins ? nil : .pins
        } else {
            showsPinsSheet = true
        }
    }
}
