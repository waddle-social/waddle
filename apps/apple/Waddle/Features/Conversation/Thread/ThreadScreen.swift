import SwiftUI
import WaddleKit

/// A XEP-0201 thread: the root message, its replies, and a composer that
/// replies inside the thread. Pushed on iPhone, shown in the inspector on
/// iPad and Mac.
struct ThreadScreen: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID
    let rootID: String

    init(conversation: ConversationID, rootID: String) {
        self.conversation = conversation
        self.rootID = rootID
    }

    var body: some View {
        ThreadContent(
            conversation: conversation,
            rootID: rootID,
            composer: ComposerDraftStore.shared.model(for: ComposerDraftStore.Key(
                account: session.account.jid,
                conversation: conversation,
                thread: rootID
            ))
        )
        .id(rootID)
    }
}

private struct ThreadContent: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var actions: MessageActionModel

    let conversation: ConversationID
    let rootID: String
    let composer: ComposerModel

    init(conversation: ConversationID, rootID: String, composer: ComposerModel) {
        self.conversation = conversation
        self.rootID = rootID
        self.composer = composer
        _actions = State(initialValue: MessageActionModel(conversation: conversation, composer: composer, isThread: true))
    }

    var body: some View {
        let timeline = session.timelines.timeline(for: conversation)
        let root = ThreadLookup.root(rootID, in: timeline)
        let replies = timeline.threadReplies(threadID: rootID)
        let entries = TimelineFeedLayout.entries(
            for: (root.map { [$0] } ?? []) + replies,
            unreadAnchorID: nil,
            groupingWindow: Theme.groupingWindow,
            replyParent: { timeline.item(withID: $0) }
        )
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if root == nil {
                        missingRoot
                    }
                    ForEach(entries) { entry in
                        MessageRow(entry: entry, showsThreadChip: false)
                            .id(entry.id)
                        if entry.id == root?.id {
                            ThreadRepliesDivider(count: replies.count)
                        }
                    }
                }
                .frame(maxWidth: Theme.Size.readableWidth)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.s)
            }
            .defaultScrollAnchor(.bottom)
            .onChange(of: replies.last?.id) { _, last in
                guard let last else { return }
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.25)) {
                    proxy.scrollTo(last, anchor: .bottom)
                }
            }
            .onChange(of: actions.scrollRequest) { _, target in
                guard let target else { return }
                actions.scrollRequest = nil
                withAnimation(reduceMotion ? nil : .easeInOut) {
                    proxy.scrollTo(target, anchor: .center)
                }
                actions.highlight(target)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            ConversationComposer(
                model: composer,
                conversation: conversation,
                thread: rootID,
                placeholder: "Reply in thread"
            )
        }
        .navigationTitle("Thread")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .environment(actions)
        .modifier(MessageActionDialogs(actions: actions))
    }

    private var missingRoot: some View {
        Label("The original message isn't loaded", systemImage: "text.bubble")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .padding(Theme.Spacing.l)
    }
}

/// "3 replies" rule between the root and its replies.
private struct ThreadRepliesDivider: View {
    let count: Int

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Text(count == 1 ? "1 reply" : "\(count) replies")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .fixedSize()
            Rectangle()
                .fill(Color.secondary.opacity(0.2))
                .frame(height: 1)
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.s)
        .accessibilityElement(children: .combine)
    }
}

/// Finds a thread's root row: the row the thread id names (a thread
/// started from a message uses its id), else the first feed row carrying
/// that XEP-0201 thread.
enum ThreadLookup {
    @MainActor
    static func root(_ threadID: String, in timeline: ConversationTimeline) -> TimelineItem? {
        if let item = timeline.item(withID: threadID) {
            return item
        }
        return timeline.items.first { $0.isFeedVisible && $0.message.thread == threadID }
    }
}
