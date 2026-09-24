import Foundation

/// Client-local slash commands available in every composer. They add no
/// wire shapes: `/me` and `/shrug` send ordinary bodies (XEP-0245 for
/// `/me`), the rest only drive local UI (GIF search, manual presence).
public enum BuiltinSlashCommand: String, CaseIterable, Hashable, Sendable {
    case me
    case shrug
    case giphy
    case away
    case active
    case dnd

    /// The canonical command word, without the `/`.
    public var name: String { rawValue }

    /// Other words the command answers to.
    public var aliases: [String] {
        switch self {
        case .giphy: return ["gif"]
        case .me, .shrug, .away, .active, .dnd: return []
        }
    }

    /// Every (lowercase) word the command answers to.
    public var keywords: [String] { [name] + aliases }

    /// Usage line shown in the popover, e.g. `/giphy [search]`.
    public var usage: String {
        switch self {
        case .me: return "/me <action>"
        case .shrug: return "/shrug [message]"
        case .giphy: return "/giphy [search]"
        case .away, .active, .dnd: return "/" + name
        }
    }

    public var description: String {
        switch self {
        case .me: return "Send an action message, e.g. /me waves"
        case .shrug: return "Append ¯\\_(ツ)_/¯ to your message"
        case .giphy: return "Search for a GIF"
        case .away: return "Set your status to Away"
        case .active: return "Set your status to Available"
        case .dnd: return "Set your status to Do Not Disturb"
        }
    }

    /// Case-insensitive exact lookup by name or alias.
    public init?(keyword: String) {
        let needle = keyword.lowercased()
        guard let match = Self.allCases.first(where: { $0.keywords.contains(needle) }) else { return nil }
        self = match
    }
}
