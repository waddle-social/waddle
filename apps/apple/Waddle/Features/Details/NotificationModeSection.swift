import SwiftUI
import WaddleKit

/// XEP-0492 notification mode picker. The coordinator applies the change
/// optimistically and rolls it back if the server refuses; the error then
/// shows under the picker.
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
                Label("Notify me about", systemImage: NotifyModeLabels.symbol(current))
            }
            .pickerStyle(.menu)
        } header: {
            Text("Notifications")
        } footer: {
            if let errorMessage {
                Text(errorMessage)
                    .foregroundStyle(.red)
            }
        }
    }

    private var modeBinding: Binding<NotifyMode> {
        Binding(
            get: { session.directory.notifyMode(for: conversation) },
            set: { save($0) }
        )
    }

    private func save(_ mode: NotifyMode) {
        guard mode != session.directory.notifyMode(for: conversation) else { return }
        errorMessage = nil
        let session = self.session
        let conversation = self.conversation
        Task {
            do {
                try await session.setNotifyMode(mode, for: conversation)
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't change notifications. Try again.")
            }
        }
    }
}
