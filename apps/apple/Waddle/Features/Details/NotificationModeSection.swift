import SwiftUI
import WaddleKit

/// The conversation's notification settings: which messages notify, and
/// whether a push notification shows them. The coordinator applies a
/// change optimistically and rolls it back if the server refuses; the
/// error then shows under the controls.
struct NotificationModeSection: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID
    @State private var save: NotificationSettingsSave

    init(conversation: ConversationID, save: NotificationSettingsSave? = nil) {
        self.conversation = conversation
        _save = State(initialValue: save ?? NotificationSettingsSave())
    }

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
            .disabled(save.isSaving)
            FormToggle("Show messages in notifications", symbol: "text.bubble", isOn: showsMessagesBinding)
                .disabled(save.isSaving)
        } header: {
            FormSectionHeader("Notifications")
        } footer: {
            VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                Text(Self.showsMessagesFooter)
                    .foregroundStyle(Theme.Colors.fgMuted)
                if save.isSaving {
                    FormSectionFooter("Saving…")
                } else if let errorMessage = save.errorMessage {
                    saveErrorBanner(errorMessage)
                }
            }
            .huddleText(Theme.Typography.caption)
        }
        .onChange(of: save.errorMessage) { _, message in
            if let message { AccessibilityNotification.Announcement(message).post() }
        }
    }

    static let showsMessagesFooter = "When on, notifications sent to your devices while Waddle is closed show who wrote and what they said. This applies to all your devices, whatever their preview setting, and the text passes through Apple's notification service."

    private var modeBinding: Binding<NotifyMode> {
        Binding(
            get: { session.directory.notifyMode(for: conversation) },
            set: { saveMode($0) }
        )
    }

    private var showsMessagesBinding: Binding<Bool> {
        Binding(
            get: { session.directory.showsMessagesInPush(for: conversation) },
            set: { savePreviews($0) }
        )
    }

    private func saveMode(_ mode: NotifyMode) {
        guard mode != session.directory.notifyMode(for: conversation) else { return }
        perform { session, conversation in try await session.setNotifyMode(mode, for: conversation) }
    }

    private func savePreviews(_ showsMessages: Bool) {
        guard showsMessages != session.directory.showsMessagesInPush(for: conversation) else { return }
        perform { session, conversation in try await session.setShowsMessagesInPush(showsMessages, for: conversation) }
    }

    private func perform(_ change: @escaping @MainActor (SessionCoordinator, ConversationID) async throws -> Void) {
        let session = self.session
        let conversation = self.conversation
        Task {
            await save.perform(for: conversation) {
                try await change(session, conversation)
            }
        }
    }

    private func saveErrorBanner(_ message: String) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium)
        return Text(message)
            .foregroundStyle(Theme.Colors.dangerText)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Theme.Spacing.m)
            .padding(.vertical, Theme.Spacing.s)
            .background(Theme.Colors.dangerSubtle, in: shape)
            .overlay { shape.strokeBorder(Theme.Colors.danger, lineWidth: WaddleTokens.BorderWidth.hair) }
            .fixedSize(horizontal: false, vertical: true)
    }
}
