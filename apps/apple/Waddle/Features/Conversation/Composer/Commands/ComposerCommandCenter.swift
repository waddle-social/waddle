import Foundation
import Observation
import WaddleKit

/// Runs what a slash command asks for beyond sending text: manual
/// presence and XEP-0050 extension commands with their form stages, and
/// reports the outcome as a composer notice.
@MainActor
@Observable
final class ComposerCommandCenter {
    /// The form stage the sheet shows.
    var stage: ExtensionCommandStage?
    var notice: ComposerNotice?
    /// Why the last form submission was refused or failed.
    private(set) var formError: String?
    /// The extension command whose first request is in flight.
    private(set) var running: ExtensionCommand?
    private(set) var isSubmitting = false

    // MARK: - Presence

    /// `/away`, `/active`, `/dnd`: keeps the current status message.
    func setAvailability(_ availability: Availability, session: SessionCoordinator) async {
        await session.setAvailability(availability, statusText: session.status.statusText)
        let title = ProfileAvailabilityOption(availability).title
        notice = ComposerNotice(severity: .info, text: "Your status is now \(title).")
    }

    // MARK: - Extension commands

    /// Starts `command` as the slash invocation asks, then shows its form
    /// stage or reports its outcome.
    func run(_ command: ExtensionCommand, invocation: ExtensionInvocation, room: BareJID?, session: SessionCoordinator) async {
        running = command
        notice = nil
        defer { running = nil }
        do {
            let result = try await start(command, invocation: invocation, room: room, session: session)
            show(result, for: command, room: room)
        } catch {
            let message = ActionErrorCopy.message(for: error, fallback: "Couldn't run \(command.name).")
            notice = ComposerNotice(severity: .error, text: message)
        }
    }

    /// Advances the shown stage with `action`.
    func submit(_ action: ExtensionCommandAction, session: SessionCoordinator) async {
        guard let current = stage, !isSubmitting else { return }
        isSubmitting = true
        formError = nil
        defer { isSubmitting = false }
        do {
            let result = try await session.submitExtensionCommand(
                current.command,
                continuing: current.result,
                action: action,
                room: current.room
            )
            // The sheet was dismissed while the stage was in flight.
            guard stage?.id == current.id else { return }
            advance(current, to: result)
        } catch let error as ExtensionCommandSubmitError {
            formError = ExtensionCommandCopy.message(for: error, in: current.result.form)
        } catch {
            formError = ActionErrorCopy.message(for: error, fallback: "Couldn't submit the form. Try again.")
        }
    }

    /// Edits a field of the shown stage.
    func setValues(_ values: [String], for variable: String) {
        guard let form = stage?.result.form else { return }
        stage?.result.form = form.setting(values, for: variable)
    }

    /// Closes the sheet; a stage still awaiting input is cancelled on the
    /// service (XEP-0050: `cancel` is always allowed).
    func dismiss(session: SessionCoordinator) {
        guard let current = stage else { return }
        stage = nil
        formError = nil
        guard current.isPending, current.result.sessionID != nil else { return }
        Task {
            _ = try? await session.submitExtensionCommand(
                current.command,
                continuing: current.result,
                action: .cancel,
                room: current.room
            )
        }
    }

    func dismissNotice() {
        notice = nil
    }

    private func start(
        _ command: ExtensionCommand,
        invocation: ExtensionInvocation,
        room: BareJID?,
        session: SessionCoordinator
    ) async throws -> ExtensionCommandResult {
        switch invocation {
        case .execute:
            return try await session.runExtensionCommand(command, room: room)
        case let .inlineSubmit(field, value):
            switch try await session.runInline(command, field: field, value: value, room: room) {
            case let .finished(result), let .needsInput(result):
                return result
            }
        case let .openForm(prefill):
            var result = try await session.runExtensionCommand(command, room: room)
            if let prefill, let form = result.form {
                result.form = form.prefillingFirstRequired(with: prefill)
            }
            return result
        }
    }

    private func show(_ result: ExtensionCommandResult, for command: ExtensionCommand, room: BareJID?) {
        if ExtensionCommandStage.needsSheet(result) {
            formError = nil
            stage = ExtensionCommandStage(command: command, room: room, result: result)
        } else {
            notice = ComposerNotice.outcome(of: result, command: command)
        }
    }

    private func advance(_ current: ExtensionCommandStage, to result: ExtensionCommandResult) {
        if ExtensionCommandStage.needsSheet(result) {
            stage?.result = result
        } else {
            stage = nil
            notice = ComposerNotice.outcome(of: result, command: current.command)
        }
    }
}
