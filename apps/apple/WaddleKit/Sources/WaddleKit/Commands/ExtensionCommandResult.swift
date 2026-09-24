import Foundation

/// A command response: its status, the stage's form and the actions the
/// service allows next.
public struct ExtensionCommandResult: Sendable, Equatable, Hashable {
    public let status: ExtensionCommandStatus
    /// XEP-0050 `sessionid`, threaded verbatim into the next stage.
    public let sessionID: String?
    public let actions: [ExtensionCommandAction]
    /// Mutable so the palette can edit the stage before submitting it.
    public var form: ExtensionCommandForm?
    public let notes: [ExtensionCommandNote]

    public init(
        status: ExtensionCommandStatus,
        sessionID: String?,
        actions: [ExtensionCommandAction],
        form: ExtensionCommandForm?,
        notes: [ExtensionCommandNote]
    ) {
        self.status = status
        self.sessionID = sessionID
        self.actions = actions
        self.form = form
        self.notes = notes
    }

    /// The form of a stage that still awaits input: an executing session
    /// whose form has fields to show.
    /// The service holds the session open for the next action, with or
    /// without a form to fill (XEP-0050 `status='executing'`).
    public var awaitsAction: Bool {
        status == .executing && sessionID != nil
    }

    public var pendingForm: ExtensionCommandForm? {
        guard status == .executing, sessionID != nil, let form, !form.fields.isEmpty else { return nil }
        return form
    }
}
