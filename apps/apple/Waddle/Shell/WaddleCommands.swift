import SwiftUI
import WaddleKit

/// Menu-bar commands (Mac) and hardware-keyboard shortcuts (iPad).
struct WaddleCommands: Commands {
    let appState: AppState

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Message") { navigation?.sheet = .newMessage }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(navigation == nil)
            Button("New Room") { navigation?.sheet = .newChannel }
                .keyboardShortcut("n", modifiers: [.command, .shift])
                .disabled(navigation == nil)
        }

        CommandMenu("Go") {
            Button("Jump to Conversation…") { navigation?.isQuickSwitcherPresented = true }
                .keyboardShortcut("k", modifiers: .command)
                .disabled(navigation == nil)
            Button("Search in Conversation…") {
                if let selection = navigation?.selection {
                    navigation?.sheet = .search(selection)
                }
            }
            .keyboardShortcut("f", modifiers: .command)
            .disabled(navigation?.selection == nil)
            Divider()
            Button("Previous Conversation") { step(by: -1) }
                .keyboardShortcut(.upArrow, modifiers: .option)
                .disabled(navigation == nil)
            Button("Next Conversation") { step(by: 1) }
                .keyboardShortcut(.downArrow, modifiers: .option)
                .disabled(navigation == nil)
            Button("Next Unread") { openNextUnread() }
                .keyboardShortcut(.downArrow, modifiers: [.option, .shift])
                .disabled(navigation == nil)
            Button("Conversation Details") {
                if let selection = navigation?.selection {
                    navigation?.showDetails(of: selection, usesInspector: true)
                }
            }
            .keyboardShortcut("i", modifiers: .command)
            .disabled(navigation?.selection == nil)
        }

        CommandGroup(after: .appSettings) {
            Button("Profile & Status…") { navigation?.sheet = .profile }
                .disabled(navigation == nil)
        }
    }

    private var navigation: NavigationModel? {
        appState.session?.navigation
    }

    /// Every conversation in sidebar order: rooms, group chats, direct messages.
    private func orderedConversations(_ session: ActiveSession) -> [ConversationID] {
        let directory = session.coordinator.directory
        return directory.channels.map(\.conversation)
            + directory.groupDMs.map(\.conversation)
            + directory.directConversations.map(\.conversation)
    }

    /// Moves the selection up or down the sidebar, wrapping at the ends.
    /// The sidebar rows are buttons, so this stands in for arrow keys.
    private func step(by offset: Int) {
        guard let session = appState.session else { return }
        let ordered = orderedConversations(session)
        guard !ordered.isEmpty else { return }
        let current = session.navigation.selection.flatMap { ordered.firstIndex(of: $0) }
        let next = current.map { ($0 + offset + ordered.count) % ordered.count } ?? (offset > 0 ? 0 : ordered.count - 1)
        session.navigation.open(ordered[next])
    }

    private func openNextUnread() {
        guard let session = appState.session else { return }
        let coordinator = session.coordinator
        let ordered = orderedConversations(session)
        let current = session.navigation.selection
        let start = current.flatMap { ordered.firstIndex(of: $0) }.map { $0 + 1 } ?? 0
        let rotated = Array(ordered[start...]) + Array(ordered[..<start])
        if let next = rotated.first(where: { coordinator.unread.count(for: $0) > 0 }) {
            session.navigation.open(next)
        }
    }
}
