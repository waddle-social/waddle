import Foundation

/// The command an exactly-typed `/prefix` runs on submit.
public enum SlashResolution: Hashable, Sendable {
    case builtin(BuiltinSlashCommand)
    case `extension`(ExtensionCommand)

    public func action(trailing: String) -> SlashAction {
        switch self {
        case let .builtin(command):
            return command.action(trailing: trailing)
        case let .extension(command):
            return .runExtension(command, ExtensionInvocation(command: command, trailing: trailing))
        }
    }
}
