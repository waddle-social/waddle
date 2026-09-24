import Foundation

/// One row of the slash popover.
public enum SlashCandidate: Hashable, Sendable {
    case builtin(BuiltinSlashCommand)
    case `extension`(ExtensionCommand)

    /// The command word the candidate completes to, without the `/`.
    public var name: String {
        switch self {
        case let .builtin(command): return command.name
        case let .extension(command): return command.composerPrefix ?? ""
        }
    }

    public var usage: String {
        switch self {
        case let .builtin(command): return command.usage
        case .extension: return "/" + name
        }
    }

    public var description: String {
        switch self {
        case let .builtin(command): return command.description
        case let .extension(command): return command.name
        }
    }
}
