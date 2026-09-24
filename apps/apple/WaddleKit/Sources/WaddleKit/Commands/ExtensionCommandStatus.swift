import Foundation

/// XEP-0050 §3 `status` of a command response.
public enum ExtensionCommandStatus: Sendable, Equatable, Hashable {
    /// The command awaits another stage.
    case executing
    case completed
    case canceled
}
