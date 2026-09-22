import SwiftUI
import WaddleKit

/// ⌘K: type to filter every conversation, ↑/↓ to move, Return to open,
/// Escape to close.
struct QuickSwitcher: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var highlighted: ConversationID?
    @FocusState private var isSearchFocused: Bool

    init() {}

    var body: some View {
        let entries = rankedEntries
        let current = QuickSwitcherCursor.resolved(highlighted, in: entries.map(\.id))
        VStack(spacing: 0) {
            searchField(entries: entries, current: current)
            Divider()
            QuickSwitcherResults(
                entries: entries,
                highlighted: current,
                query: query,
                onHover: { highlighted = $0 },
                onOpen: { open($0) }
            )
        }
        .quickSwitcherPresentation()
        .onAppear { isSearchFocused = true }
        .onChange(of: query) { _, _ in
            highlighted = nil
        }
    }

    private func searchField(entries: [QuickSwitcherEntry], current: ConversationID?) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            TextField("Jump to a conversation", text: $query)
                .textFieldStyle(.plain)
                .font(.title3)
                .autocorrectionDisabled()
                .focused($isSearchFocused)
                .onSubmit { openHighlighted(current) }
                .onKeyPress(.upArrow) {
                    highlighted = QuickSwitcherCursor.moved(current, by: -1, in: entries.map(\.id))
                    return .handled
                }
                .onKeyPress(.downArrow) {
                    highlighted = QuickSwitcherCursor.moved(current, by: 1, in: entries.map(\.id))
                    return .handled
                }
                .onKeyPress(.escape) {
                    dismiss()
                    return .handled
                }
            Button("Close") { dismiss() }
                .keyboardShortcut(.cancelAction)
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.m)
    }

    private var rankedEntries: [QuickSwitcherEntry] {
        let directory = session.directory
        let all = QuickSwitcherRanking.entries(
            channels: directory.channels,
            groupDMs: directory.groupDMs,
            directs: directory.directConversations,
            spaces: directory.spaces,
            unread: session.unread.counts,
            mentions: session.unread.mentions
        )
        return QuickSwitcherRanking.ranked(all, query: query)
    }

    private func openHighlighted(_ current: ConversationID?) {
        guard let current else { return }
        open(current)
    }

    private func open(_ conversation: ConversationID) {
        navigation.open(conversation)
        dismiss()
    }
}

private extension View {
    func quickSwitcherPresentation() -> some View {
        #if os(iOS)
        return self.presentationDetents([.medium, .large])
        #else
        return self.frame(minWidth: 480, idealWidth: 560, minHeight: 340, idealHeight: 420)
        #endif
    }
}
