import Foundation

/// Where an extension command may run.
public enum ExtensionCommandScope: Sendable, Equatable, Hashable {
    /// Anywhere, including 1:1 conversations.
    case global
    /// Only inside a room.
    case channel
}

/// A XEP-0050 ad-hoc command a server extension advertises, with the
/// composer hints the slash popover uses.
public struct ExtensionCommand: Sendable, Equatable, Hashable {
    /// The entity that executes the command.
    public let serviceJID: String
    /// The XEP-0050 command node.
    public let node: String
    /// Human-readable command name.
    public let name: String
    public let scope: ExtensionCommandScope
    /// The `/word` that invokes the command from the composer, if any.
    public let composerPrefix: String?
    /// The form field the composer's trailing text fills in directly.
    public let inlineField: String?
    /// Whether a bare `/word` executes without showing a form.
    public let composerExecute: Bool

    public init(
        serviceJID: String,
        node: String,
        name: String,
        scope: ExtensionCommandScope,
        composerPrefix: String?,
        inlineField: String?,
        composerExecute: Bool
    ) {
        self.serviceJID = serviceJID
        self.node = node
        self.name = name
        self.scope = scope
        self.composerPrefix = composerPrefix
        self.inlineField = inlineField
        self.composerExecute = composerExecute
    }
}
