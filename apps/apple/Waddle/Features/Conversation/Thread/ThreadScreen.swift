import SwiftUI
import WaddleKit

/// A XEP-0201 thread: the root message, its replies, and a composer that
/// replies inside the thread. Pushed on iPhone, shown in the inspector on
/// iPad and Mac. A reply's sub-thread is a thread like any other, rooted at
/// that reply, with a way back to the thread it hangs off.
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
    @Environment(AppState.self) private var app
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var actions: MessageActionModel
    private var placement = ConversationInspectorPlacement()

    let conversation: ConversationID
    let rootID: String
    let composer: ComposerModel

    init(conversation: ConversationID, rootID: String, composer: ComposerModel) {
        self.conversation = conversation
        self.rootID = rootID
        self.composer = composer
        _actions = State(initialValue: MessageActionModel(conversation: conversation, composer: composer, thread: rootID))
    }

    var body: some View {
        let timeline = session.timelines.timeline(for: conversation)
        // A room thread also shows replies fetched with the MAM thread
        // filter, so a thread opened from Activity is complete even when
        // the room's loaded page does not reach back to it.
        let fetched = roomThread.flatMap { session.threadHistory.history(for: $0) }
        let root = ThreadLookup.root(rootID, in: timeline) ?? fetched?.root
        let replies = ThreadHistory.merged(live: timeline.threadReplies(threadID: rootID), fetched: fetched?.replies ?? [])
        let parent = ThreadLookup.parent(of: rootID, in: timeline, fetched: roomThread.flatMap(session.threadHistory.parent(of:)))
        // The social order shows the newest reply first and the root,
        // the oldest message, last.
        let newestFirst = app.preferences.messageOrder.isNewestFirst
        let entries = TimelineFeedLayout.entries(
            for: (root.map { [$0] } ?? []) + replies,
            unreadAnchorID: nil,
            groupingWindow: Theme.groupingWindow,
            newestFirst: newestFirst,
            thread: { subThread(of: $0, in: timeline) },
            replyParent: { timeline.item(withID: $0) }
        )
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if root == nil, !newestFirst {
                        missingRoot
                    }
                    ForEach(entries) { entry in
                        let isRoot = entry.id == root?.presentationID
                        if isRoot, newestFirst {
                            ThreadRepliesDivider(count: replies.count)
                        }
                        MessageRow(entry: entry, showsThreadChip: false)
                            .id(entry.id)
                        if isRoot, !newestFirst {
                            ThreadRepliesDivider(count: replies.count)
                        }
                    }
                    if root == nil, newestFirst {
                        missingRoot
                    }
                }
                .frame(maxWidth: Theme.Size.readableWidth)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.s)
            }
            .defaultScrollAnchor(newestFirst ? UnitPoint.top : UnitPoint.bottom)
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: replies.last?.presentationID) { _, last in
                guard let last else { return }
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.25)) {
                    proxy.scrollTo(last, anchor: newestFirst ? UnitPoint.top : UnitPoint.bottom)
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
        .safeAreaInset(edge: .top, spacing: 0) {
            VStack(spacing: 0) {
                if let parent {
                    ThreadParentBar(snippet: parentSnippet(parent, in: timeline)) {
                        navigation.openParentThread(parent, of: rootID, in: conversation, usesInspector: placement.usesInspector)
                    }
                }
                if app.preferences.messageOrder.isNewestFirst {
                    replyComposer(parent: parent)
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if !app.preferences.messageOrder.isNewestFirst {
                replyComposer(parent: parent)
            }
        }
        .navigationTitle(parent == nil ? "Thread" : "Sub-thread")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .environment(actions)
        .modifier(MessageActionDialogs(actions: actions))
        .task(id: roomThread) {
            guard let roomThread else { return }
            await session.openThread(roomThread)
        }
        .onDisappear {
            if let roomThread {
                session.closeThread(roomThread)
            }
        }
    }

    /// A sub-thread's messages carry the thread it hangs off as their
    /// XEP-0201 `parent`.
    private func replyComposer(parent: String?) -> some View {
        ConversationComposer(
            model: composer,
            conversation: conversation,
            thread: rootID,
            parentThread: parent,
            placeholder: "Reply in thread"
        )
    }

    /// A reply's sub-thread: the timeline's messages under it, plus any
    /// fetched when that sub-thread was last open. Nil for the root.
    private func subThread(of item: TimelineItem, in timeline: ConversationTimeline) -> ThreadSummary? {
        guard let id = item.threadTarget(inside: rootID) else { return nil }
        let fetched = conversation.isRoom
            ? session.threadHistory.history(for: ThreadKey(room: conversation.jid, threadID: id))?.replies ?? []
            : []
        return ThreadSummary.of(replies: ThreadHistory.merged(live: timeline.threadDescendants(of: item), fetched: fetched))
    }

    /// The parent thread's root as one line, when it is loaded.
    private func parentSnippet(_ parent: String, in timeline: ConversationTimeline) -> String? {
        let fetched = conversation.isRoom
            ? session.threadHistory.history(for: ThreadKey(room: conversation.jid, threadID: parent))?.root
            : nil
        guard let root = ThreadLookup.root(parent, in: timeline) ?? fetched else { return nil }
        let snippet = MessageThreadCardText.snippet(ThreadSummary.snippet(of: root))
        return snippet.isEmpty ? nil : snippet
    }

    /// Room threads have their own inbox row and a MAM thread filter;
    /// direct-message threads have neither.
    private var roomThread: ThreadKey? {
        conversation.isRoom ? ThreadKey(room: conversation.jid, threadID: rootID) : nil
    }

    private var missingRoot: some View {
        Label("The original message isn't loaded", systemImage: "text.bubble")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .padding(Theme.Spacing.l)
    }
}

/// "In thread: <the parent's first message>", above a sub-thread: where
/// it hangs off, and the way back there.
private struct ThreadParentBar: View {
    let snippet: String?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: Theme.Spacing.xs) {
                Image(systemName: "chevron.left")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Color.accentColor)
                Text(snippet.map { "In thread: \($0)" } ?? "In thread")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.Spacing.l)
            .frame(minHeight: Self.minHeight)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(.bar)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Theme.Colors.borderSoft)
                .frame(height: 1)
        }
        .accessibilityLabel(Text(snippet.map { "Back to thread: \($0)" } ?? "Back to thread"))
    }

    #if os(iOS)
    private static let minHeight = Theme.Size.minHitTarget
    #else
    private static let minHeight = WaddleTokens.ControlHeight.sm
    #endif
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

    /// The thread a sub-thread hangs off: the timeline's answer when it
    /// has the sub-thread's root, else `fetched`, what fetched thread
    /// pages say.
    @MainActor
    static func parent(of threadID: String, in timeline: ConversationTimeline, fetched: String?) -> String? {
        if timeline.item(withID: threadID) != nil {
            return timeline.parentThread(of: threadID)
        }
        return fetched
    }
}
