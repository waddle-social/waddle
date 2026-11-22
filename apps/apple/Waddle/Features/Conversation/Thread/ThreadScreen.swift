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
    /// A fixed clock, for snapshots; the app follows the real one.
    var clock: Date?
    /// A row whose hover toolbar shows without a pointer, for snapshots.
    var toolbarRow: String?

    init(conversation: ConversationID, rootID: String, clock: Date? = nil, toolbarRow: String? = nil) {
        self.conversation = conversation
        self.rootID = rootID
        self.clock = clock
        self.toolbarRow = toolbarRow
    }

    var body: some View {
        ThreadContent(
            conversation: conversation,
            rootID: rootID,
            composer: ComposerDraftStore.shared.model(for: ComposerDraftStore.Key(
                account: session.account.jid,
                conversation: conversation,
                thread: rootID
            )),
            clock: clock,
            toolbarRow: toolbarRow
        )
        .id(rootID)
    }
}

private struct ThreadContent: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(AppState.self) private var app
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// In the iPad or Mac inspector rather than pushed on a stack.
    @Environment(\.isInInspector) private var isInInspector
    @State private var actions: MessageActionModel

    let conversation: ConversationID
    let rootID: String
    let composer: ComposerModel
    let clock: Date?

    init(conversation: ConversationID, rootID: String, composer: ComposerModel, clock: Date?, toolbarRow: String?) {
        self.conversation = conversation
        self.rootID = rootID
        self.composer = composer
        self.clock = clock
        let actions = MessageActionModel(conversation: conversation, composer: composer, thread: rootID)
        if let toolbarRow {
            actions.holdToolbar(of: toolbarRow)
        }
        _actions = State(initialValue: actions)
    }

    var body: some View {
        let timeline = session.timelines.timeline(for: conversation)
        // A room thread also shows replies fetched with the MAM thread
        // filter, so a thread opened from Activity is complete even when
        // the room's loaded page does not reach back to it.
        let fetched = fetchedThread(rootID)
        // A sub-thread just started on a reply that only its parent's fetched
        // page holds has no page of its own yet; that reply is its root.
        let root = session.threadRoot(of: rootID, in: conversation)
        let replies = ThreadHistory.merged(live: timeline.threadReplies(threadID: rootID), fetched: fetched?.replies ?? [])
        // Under the root, as on the feed's card and on web: every message
        // in the thread, its sub-threads' included.
        // Merged with the fetched replies only: `replies` already holds the
        // live ones, and a 1:1 merge does not match copies.
        let replyCount = root.map {
            ThreadHistory.merged(live: timeline.threadDescendants(of: $0), fetched: fetched?.replies ?? []).count
        }
            ?? replies.count
        let header = ThreadHeader(
            root: root,
            replies: replies,
            replyCount: replyCount,
            breadcrumb: session.threadBreadcrumb(of: rootID, in: conversation)
        )
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
                            ThreadRepliesDivider(count: replyCount)
                        }
                        MessageRow(entry: entry, showsThreadChip: false)
                            .id(entry.id)
                        if isRoot, !newestFirst {
                            ThreadRepliesDivider(count: replyCount)
                        }
                    }
                    if root == nil, newestFirst {
                        missingRoot
                    }
                }
                .frame(maxWidth: Theme.Size.readableWidth)
                .frame(maxWidth: .infinity)
                // Room above the first row for its hover toolbar.
                .padding(.top, Theme.Spacing.l)
                .padding(.bottom, Theme.Spacing.s)
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
                ThreadHeaderView(
                    header: header,
                    clock: clock,
                    // On a phone stack whose Back already goes to the
                    // parent, the header needs no second way there.
                    showsBack: isInInspector || navigation.threadBelow(rootID, in: conversation) != header.parent?.threadID,
                    open: { thread in
                        navigation.openEnclosingThread(
                            thread,
                            along: header.breadcrumb.map(\.threadID),
                            in: conversation,
                            usesInspector: isInInspector
                        )
                    },
                    close: isInInspector ? { navigation.inspector = nil } : nil
                )
                if app.preferences.messageOrder.isNewestFirst {
                    replyComposer(isSubThread: header.isSubThread)
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if !app.preferences.messageOrder.isNewestFirst {
                replyComposer(isSubThread: header.isSubThread)
            }
        }
        .modifier(ThreadNavigationChrome(isInspector: isInInspector, isSubThread: header.isSubThread))
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

    /// The session adds a sub-thread's XEP-0201 `parent` to each send.
    private func replyComposer(isSubThread: Bool) -> some View {
        ConversationComposer(
            model: composer,
            conversation: conversation,
            thread: rootID,
            placeholder: isSubThread ? "Reply in sub-thread" : "Reply in thread"
        )
    }

    /// A reply's sub-thread: the timeline's messages under it, plus any
    /// fetched when that sub-thread was last open. Nil for the root.
    private func subThread(of item: TimelineItem, in timeline: ConversationTimeline) -> ThreadSummary? {
        guard let id = item.threadTarget(inside: rootID) else { return nil }
        let fetched = fetchedThread(id)?.replies ?? []
        return ThreadSummary.of(replies: ThreadHistory.merged(live: timeline.threadDescendants(of: item), fetched: fetched))
    }

    /// Room threads have their own inbox row and a MAM thread filter;
    /// direct-message threads have neither.
    private var roomThread: ThreadKey? {
        roomThread(rootID)
    }

    private func roomThread(_ id: String) -> ThreadKey? {
        conversation.isRoom ? ThreadKey(room: conversation.jid, threadID: id) : nil
    }

    /// What the MAM thread filter fetched for the room thread `id`.
    private func fetchedThread(_ id: String) -> ThreadHistory? {
        roomThread(id).flatMap { session.threadHistory.history(for: $0) }
    }

    private var missingRoot: some View {
        Label("The original message isn't loaded", systemImage: "text.bubble")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .padding(Theme.Spacing.l)
    }
}

/// The header is the screen's title in the inspector, where the bar is
/// hidden and the header's own button closes it. Pushed on a phone, the
/// navigation bar keeps its back button with a short title.
private struct ThreadNavigationChrome: ViewModifier {
    let isInspector: Bool
    let isSubThread: Bool

    func body(content: Content) -> some View {
        #if os(iOS)
        if isInspector {
            content.toolbar(.hidden, for: .navigationBar)
        } else {
            content
                .navigationTitle(isSubThread ? "Sub-thread" : "Thread")
                .navigationBarTitleDisplayMode(.inline)
        }
        #else
        content
        #endif
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
