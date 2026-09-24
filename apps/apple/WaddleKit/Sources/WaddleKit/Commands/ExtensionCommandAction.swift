import Foundation

/// XEP-0050 §3 `action`: how a command stage is advanced.
public enum ExtensionCommandAction: Sendable, Equatable, Hashable {
    case execute
    case cancel
    case next
    case prev
    case complete

    /// Whether the action moves the command forward and so carries the
    /// form. `cancel` and `prev` never submit values.
    public var submitsForm: Bool {
        switch self {
        case .execute, .next, .complete: return true
        case .cancel, .prev: return false
        }
    }
}
