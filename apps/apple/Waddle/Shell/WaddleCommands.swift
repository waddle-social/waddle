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
            Button("New Channel") { navigation?.sheet = .newChannel }
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

    private func openNextUnread() {
        guard let session = appState.session else { return }
        let coordinator = session.coordinator
        let ordered = coordinator.directory.channels.map(\.conversation)
            + coordinator.directory.groupDMs.map(\.conversation)
            + coordinator.directory.directConversations.map(\.conversation)
        let current = session.navigation.selection
        let start = current.flatMap { ordered.firstIndex(of: $0) }.map { $0 + 1 } ?? 0
        let rotated = Array(ordered[start...]) + Array(ordered[..<start])
        if let next = rotated.first(where: { coordinator.unread.count(for: $0) > 0 }) {
            session.navigation.open(next)
        }
    }
}
