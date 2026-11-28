import Observation

/// Save feedback for a conversation's notification controls.
@MainActor
@Observable
public final class NotificationSettingsSave {
    public private(set) var isSaving = false
    public private(set) var errorMessage: String?

    public init() {}

    public func perform(for conversation: ConversationID, _ change: @MainActor () async throws -> Void) async {
        guard !isSaving else { return }
        isSaving = true
        errorMessage = nil
        defer { isSaving = false }

        do {
            try await change()
        } catch {
            errorMessage = Self.message(for: error, in: conversation)
        }
    }

    private static func message(for error: Error, in conversation: ConversationID) -> String {
        guard (error as? PortError) == .notificationSettingsStorageMismatch else {
            return "Couldn't save the setting. Try again in a moment."
        }
        switch conversation.kind {
        case .room:
            return "This room's settings node was created by an older client. Ask a server admin to delete the node so Waddle can re-create it."
        case .direct:
            return "Your direct-message notification settings couldn't be saved — their storage was created by an incompatible older client."
        }
    }
}
