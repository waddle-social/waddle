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
                // Rows are keyed by their primary id; resolve any alias.
                let timeline = session.timelines.timeline(for: conversation)
                actions.scrollRequest = timeline.item(withID: request.messageID)?.id ?? request.messageID
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

    private func showPins() {
        if placement.usesInspector {
            navigation.inspector = navigation.inspector == .pins ? nil : .pins
        } else {
            showsPinsSheet = true
        }
    }
}
