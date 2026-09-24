import Foundation

/// XEP-0050 extension commands: discovery, invocation and form stages.
extension SessionCoordinator {
    /// Replaces `extensionCommands` with a fresh discovery. A failure
    /// keeps the previous list; an answer that arrives after the stream
    /// dropped or the session signed out is discarded.
    public func refreshExtensionCommands() async {
        guard connection == .online else { return }
        let epoch = connectionEpoch
        guard let commands = try? await port.discoverExtensionCommands() else { return }
        guard !Task.isCancelled, epoch == connectionEpoch else { return }
        extensionCommands = commands
    }

    /// XEP-0050 §2.4: starts `command`, in `room` when run from one.
    public func runExtensionCommand(_ command: ExtensionCommand, room: BareJID?) async throws -> ExtensionCommandResult {
        guard connection == .online else { throw PortError.notConnected }
        return try await port.invokeExtensionCommand(command, room: room)
    }

    /// XEP-0050 §3: advances `stage` (the previous response, its form
    /// edited) with `action`. Forward actions throw
    /// `ExtensionCommandSubmitError` before anything is sent while the
    /// form holds a blocked field or lacks a required value.
    public func submitExtensionCommand(
        _ command: ExtensionCommand,
        continuing stage: ExtensionCommandResult,
        action: ExtensionCommandAction,
        room: BareJID?
    ) async throws -> ExtensionCommandResult {
        let values = try stage.form?.submission(for: action) ?? []
        guard connection == .online else { throw PortError.notConnected }
        return try await port.submitExtensionCommandForm(
            command,
            sessionID: stage.sessionID,
            values: values,
            action: action,
            room: room
        )
    }

    /// Runs `command` with composer text as `field`'s value. The value is
    /// submitted with `complete` only for a single-stage form that allows
    /// it, contains `field`, and has no blocked field or other empty
    /// required field; otherwise the stage is returned prefilled for the
    /// palette.
    public func runInline(
        _ command: ExtensionCommand,
        field: String,
        value: String,
        room: BareJID?
    ) async throws -> ExtensionInlineOutcome {
        let invoked = try await runExtensionCommand(command, room: room)
        guard let form = invoked.pendingForm, form.field(field) != nil else {
            return inlineOutcome(invoked)
        }
        var stage = invoked
        let filled = form.setting([value], for: field)
        stage.form = filled
        guard invoked.actions.contains(.complete),
              filled.blockedField == nil,
              filled.missingRequiredFields.isEmpty
        else { return .needsInput(stage) }
        let submitted = try await submitExtensionCommand(command, continuing: stage, action: .complete, room: room)
        return inlineOutcome(submitted)
    }

    private func inlineOutcome(_ result: ExtensionCommandResult) -> ExtensionInlineOutcome {
        result.status == .executing ? .needsInput(result) : .finished(result)
    }
}
