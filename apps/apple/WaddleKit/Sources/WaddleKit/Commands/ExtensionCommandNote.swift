import Foundation

/// XEP-0050 §3 `<note/>` severity.
public enum ExtensionCommandNoteType: Sendable, Equatable, Hashable {
    case info
    case warn
    case error
}

/// A diagnostic the service attached to a command response.
public struct ExtensionCommandNote: Sendable, Equatable, Hashable {
    public let type: ExtensionCommandNoteType
    public let text: String

    public init(type: ExtensionCommandNoteType, text: String) {
        self.type = type
        self.text = text
    }
}
