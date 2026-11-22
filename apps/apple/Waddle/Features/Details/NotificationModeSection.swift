import SwiftUI
import WaddleKit

/// The conversation's notification settings: which messages notify, and
/// whether a push notification shows them. The coordinator applies a
/// change optimistically and rolls it back if the server refuses; the
/// error then shows under the controls.
struct NotificationModeSection: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID
    @State private var errorMessage: String?

    var body: some View {
        let current = session.directory.notifyMode(for: conversation)
        Section {
            Picker(selection: modeBinding) {
                ForEach(NotifyModeLabels.ordered, id: \.self) { mode in
                    Text(NotifyModeLabels.title(mode)).tag(mode)
                }
            } label: {
                FormRowLabel(title: "Notify me about", symbol: NotifyModeLabels.symbol(current))
            }
            .pickerStyle(.menu)
            FormToggle("Show messages in notifications", symbol: "text.bubble", isOn: showsMessagesBinding)
        } header: {
            FormSectionHeader("Notifications")
        } footer: {
            VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                Text(Self.showsMessagesFooter)
                    .foregroundStyle(Theme.Colors.fgMuted)
                if let errorMessage {
                    Text(errorMessage)
                        .foregroundStyle(Theme.Colors.dangerText)
                }
            }
            .huddleText(Theme.Typography.caption)
        }
    }

    static let showsMessagesFooter = "When on, notifications sent to your devices while Waddle is closed show who wrote and what they said. This applies to all your devices, whatever their preview setting, and the text passes through Apple's notification service."

    private var modeBinding: Binding<NotifyMode> {
        Binding(
            get: { session.directory.notifyMode(for: conversation) },
            set: { save($0) }
        )
    }

    private var showsMessagesBinding: Binding<Bool> {
        Binding(
            get: { session.directory.showsMessagesInPush(for: conversation) },
            set: { save(showsMessages: $0) }
        )
    }

    private func save(_ mode: NotifyMode) {
        guard mode != session.directory.notifyMode(for: conversation) else { return }
        perform { session, conversation in try await session.setNotifyMode(mode, for: conversation) }
    }

    private func save(showsMessages: Bool) {
        guard showsMessages != session.directory.showsMessagesInPush(for: conversation) else { return }
        perform { session, conversation in try await session.setShowsMessagesInPush(showsMessages, for: conversation) }
    }

    private func perform(_ change: @escaping @MainActor (SessionCoordinator, ConversationID) async throws -> Void) {
        errorMessage = nil
        let session = self.session
        let conversation = self.conversation
        Task {
            do {
                try await change(session, conversation)
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't change notifications. Try again.")
            }
        }
    }
}
