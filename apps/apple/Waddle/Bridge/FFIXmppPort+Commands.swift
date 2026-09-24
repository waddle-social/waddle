import Foundation
import WaddleKit

/// XEP-0050 extension commands of the server's extension service.
extension FFIXmppPort {
    func discoverExtensionCommands() async throws -> [ExtensionCommand] {
        let commands = try await mappingPortErrors { try await client.discoverExtensionCommands() }
        return commands.map(FFIInbound.extensionCommand)
    }

    func invokeExtensionCommand(_ command: ExtensionCommand, room: BareJID?) async throws -> ExtensionCommandResult {
        let result = try await mappingPortErrors {
            try await client.invokeExtensionCommand(
                serviceJid: command.serviceJID,
                node: command.node,
                roomJid: room?.description
            )
        }
        return FFIInbound.extensionCommandResult(result)
    }

    func submitExtensionCommandForm(
        _ command: ExtensionCommand,
        sessionID: String?,
        values: [ExtensionFormValue],
        action: ExtensionCommandAction,
        room: BareJID?
    ) async throws -> ExtensionCommandResult {
        let result = try await mappingPortErrors {
            try await client.submitExtensionCommandForm(
                serviceJid: command.serviceJID,
                node: command.node,
                sessionId: sessionID,
                fields: values.map(FFIOutbound.extensionFormField),
                action: FFIOutbound.adhocAction(action),
                roomJid: room?.description
            )
        }
        return FFIInbound.extensionCommandResult(result)
    }
}

extension FFIInbound {
    static func extensionCommand(_ command: WaddleExtensionCommand) -> ExtensionCommand {
        ExtensionCommand(
            serviceJID: command.serviceJid,
            node: command.node,
            name: command.name,
            scope: extensionCommandScope(command.scope),
            composerPrefix: command.composerPrefix,
            inlineField: command.inlineField,
            composerExecute: command.composerExecute
        )
    }

    static func extensionCommandResult(_ result: WaddleExtensionCommandResult) -> ExtensionCommandResult {
        ExtensionCommandResult(
            status: adhocStatus(result.status),
            sessionID: result.sessionId,
            actions: result.actions.map(adhocAction),
            form: result.form.map(extensionCommandForm),
            notes: result.notes.map(extensionCommandNote)
        )
    }

    private static func extensionCommandForm(_ form: WaddleExtensionCommandForm) -> ExtensionCommandForm {
        ExtensionCommandForm(
            title: form.title,
            instructions: form.instructions,
            fields: form.fields.map(extensionCommandField)
        )
    }

    private static func extensionCommandField(_ field: WaddleExtensionCommandFormField) -> ExtensionCommandField {
        ExtensionCommandField(
            variable: field.`var`,
            label: field.label,
            type: extensionFieldType(field.fieldType),
            required: field.required,
            blocked: field.blocked,
            options: field.options.map { ExtensionFieldOption(label: $0.label, value: $0.value) },
            values: field.values
        )
    }

    private static func extensionCommandNote(_ note: WaddleExtensionCommandNote) -> ExtensionCommandNote {
        ExtensionCommandNote(type: extensionNoteType(note.noteType), text: note.value)
    }

    private static func extensionCommandScope(_ scope: WaddleExtensionCommandScope) -> ExtensionCommandScope {
        switch scope {
        case .global: return .global
        case .channel: return .channel
        }
    }

    private static func adhocStatus(_ status: WaddleAdhocStatus) -> ExtensionCommandStatus {
        switch status {
        case .executing: return .executing
        case .completed: return .completed
        case .canceled: return .canceled
        }
    }

    private static func adhocAction(_ action: WaddleAdhocAction) -> ExtensionCommandAction {
        switch action {
        case .execute: return .execute
        case .cancel: return .cancel
        case .next: return .next
        case .prev: return .prev
        case .complete: return .complete
        }
    }

    private static func extensionFieldType(_ type: WaddleExtensionFieldType) -> ExtensionFieldType {
        switch type {
        case .boolean: return .boolean
        case .fixed: return .fixed
        case .hidden: return .hidden
        case .jidMulti: return .jidMulti
        case .jidSingle: return .jidSingle
        case .listMulti: return .listMulti
        case .listSingle: return .listSingle
        case .textMulti: return .textMulti
        case .textPrivate: return .textPrivate
        case .textSingle: return .textSingle
        }
    }

    private static func extensionNoteType(_ type: WaddleExtensionNoteType) -> ExtensionCommandNoteType {
        switch type {
        case .info: return .info
        case .warn: return .warn
        case .error: return .error
        }
    }
}

extension FFIOutbound {
    static func extensionFormField(_ value: ExtensionFormValue) -> WaddleExtensionFormField {
        WaddleExtensionFormField(`var`: value.variable, values: value.values)
    }

    static func adhocAction(_ action: ExtensionCommandAction) -> WaddleAdhocAction {
        switch action {
        case .execute: return .execute
        case .cancel: return .cancel
        case .next: return .next
        case .prev: return .prev
        case .complete: return .complete
        }
    }
}
