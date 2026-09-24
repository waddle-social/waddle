import Foundation

/// XEP-0004 §3.3 field types.
public enum ExtensionFieldType: Sendable, Equatable, Hashable {
    case boolean
    case fixed
    case hidden
    case jidMulti
    case jidSingle
    case listMulti
    case listSingle
    case textMulti
    case textPrivate
    case textSingle
}
