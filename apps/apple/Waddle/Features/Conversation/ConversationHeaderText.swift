import Foundation
import WaddleKit

/// Copy that depends on the conversation's name and kind.
struct ConversationHeaderText: Hashable {
    let name: String
    /// A space channel (shown with `#`), not a group DM or a 1:1.
    let isChannel: Bool

    var title: String { isChannel ? "#\(name)" : name }

    var composerPlaceholder: String { "Message \(title)" }

    var beginningTitle: String { "Beginning of \(title)" }

    var emptyTitle: String { isChannel ? "This is the start of \(title)" : "Say hi to \(name)" }

    var emptyMessage: String {
        isChannel ? "Messages sent here are visible to everyone in the channel." : "Messages you send appear here."
    }

    var searchPrompt: String { "Search \(title)" }

    /// Member count while joined, else the channel summary.
    static func roomSubtitle(occupantCount: Int?, summary: String?) -> String? {
        if let occupantCount, occupantCount > 0 {
            return occupantCount == 1 ? "1 member" : "\(occupantCount) members"
        }
        let trimmed = summary?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? nil : trimmed
    }

    /// RFC 6121 availability in words, with the status message when set.
    static func directSubtitle(availability: Availability, status: String?) -> String {
        let word: String
        switch availability {
        case .available, .chat: word = "Online"
        case .away: word = "Away"
        case .extendedAway: word = "Away for a while"
        case .doNotDisturb: word = "Do not disturb"
        case .offline: word = "Offline"
        }
        let trimmed = status?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? word : "\(word) · \(trimmed)"
    }
}
