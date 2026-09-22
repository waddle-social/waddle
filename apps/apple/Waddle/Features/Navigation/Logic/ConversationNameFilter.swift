import Foundation

/// Name matching for the list search fields and the quick switcher.
enum ConversationNameFilter {
    /// The query as typed, trimmed, with a leading `#` dropped so
    /// `#general` finds the `general` channel.
    static func normalized(_ query: String) -> String {
        var trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasPrefix("#") {
            trimmed.removeFirst()
        }
        return trimmed
    }

    /// Whether any candidate contains the query, ignoring case and
    /// diacritics. An empty query matches everything.
    static func matches(_ candidates: [String], query: String) -> Bool {
        let needle = normalized(query)
        guard !needle.isEmpty else { return true }
        return candidates.contains { contains($0, needle) }
    }

    static func contains(_ haystack: String, _ needle: String) -> Bool {
        haystack.range(of: needle, options: [.caseInsensitive, .diacriticInsensitive]) != nil
    }

    static func hasPrefix(_ haystack: String, _ needle: String) -> Bool {
        haystack.range(of: needle, options: [.caseInsensitive, .diacriticInsensitive, .anchored]) != nil
    }
}
