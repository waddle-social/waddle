import Foundation

/// The `@word` being typed at the end of the draft.
public struct MentionQuery: Hashable, Sendable {
    /// Scalar offset of the `@`.
    public let start: Int
    /// What follows the `@`.
    public let text: String
}

/// A mention the user picked, re-located in the final text on send.
public struct RecordedMention: Hashable, Sendable {
    /// `@nick`, as inserted.
    public let token: String
    public let target: MentionTarget

    public init(token: String, target: MentionTarget) {
        self.token = token
        self.target = target
    }
}

/// Mention token handling over the raw draft, in Unicode scalar offsets
/// (the unit XEP-0372 references count in).
public enum MentionTokens {
    /// Finds exact own-nick tokens for presentation fallback. Punctuation
    /// only ends a nick when it is not immediately followed by more nick
    /// text, so `@alice.bob` cannot highlight `@alice`. Ranges use Unicode
    /// scalar offsets.
    public static func ownNickRanges(_ nick: String, in text: String) -> [Range<Int>] {
        let tokenScalars = Array(("@" + nick).unicodeScalars)
        guard tokenScalars.count > 1 else { return [] }
        let scalars = Array(text.unicodeScalars)
        return occurrences(of: tokenScalars, in: scalars).filter {
            ownNickBoundary(after: $0.upperBound, in: scalars)
        }
    }

    /// The trailing `@word` when the draft ends inside one. The composer
    /// has no caret position on every OS it supports, so completion works
    /// on the word being typed at the end.
    public static func trailingQuery(in text: String) -> MentionQuery? {
        let scalars = Array(text.unicodeScalars)
        let tokenStart = (scalars.lastIndex(where: isSpace) ?? -1) + 1
        guard tokenStart < scalars.count, scalars[tokenStart] == "@" else { return nil }
        let query = scalars[(tokenStart + 1)...]
        guard query.count <= 64, !query.contains("@") else { return nil }
        return MentionQuery(start: tokenStart, text: string(query))
    }

    /// Replaces the query with `@name ` and returns the new text.
    public static func completing(_ text: String, query: MentionQuery, with name: String) -> String {
        let scalars = Array(text.unicodeScalars)
        let prefix = string(scalars[..<min(query.start, scalars.count)])
        return prefix + "@" + name + " "
    }

    /// Finds every recorded token still present as a whole word and returns
    /// one mention per occurrence. Longer tokens claim first so `@ann`
    /// never matches inside `@anna`. Deleted or edited tokens drop out, and
    /// so does a token recorded for more than one target (a nick reused by
    /// someone else): its occurrences cannot be told apart, and plain text
    /// is safer than mentioning the wrong person.
    public static func locate(_ recorded: [RecordedMention], in text: String) -> [MentionDraft] {
        let scalars = Array(text.unicodeScalars)
        var claimed: [Range<Int>] = []
        var mentions: [MentionDraft] = []
        let unambiguous = Dictionary(grouping: Set(recorded), by: \.token).values.compactMap { targets in
            targets.count == 1 ? targets.first : nil
        }
        let ordered = unambiguous.sorted {
            ($0.token.unicodeScalars.count, $0.token) > ($1.token.unicodeScalars.count, $1.token)
        }
        for mention in ordered {
            let token = Array(mention.token.unicodeScalars)
            guard token.count > 1 else { continue }
            for range in occurrences(of: token, in: scalars) where !claimed.contains(where: { $0.overlaps(range) }) {
                claimed.append(range)
                mentions.append(MentionDraft(target: mention.target, range: range))
            }
        }
        return mentions.sorted { $0.range.lowerBound < $1.range.lowerBound }
    }

    private static func occurrences(of token: [Unicode.Scalar], in scalars: [Unicode.Scalar]) -> [Range<Int>] {
        guard token.count <= scalars.count else { return [] }
        var ranges: [Range<Int>] = []
        var index = 0
        while index + token.count <= scalars.count {
            let end = index + token.count
            if scalars[index..<end].elementsEqual(token),
               index == 0 || opensWord(scalars[index - 1]),
               end == scalars.count || closesWord(scalars[end]) {
                ranges.append(index..<end)
                index = end
            } else {
                index += 1
            }
        }
        return ranges
    }

    private static func string(_ scalars: ArraySlice<Unicode.Scalar>) -> String {
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars)
        return String(view)
    }

    private static func isSpace(_ scalar: Unicode.Scalar) -> Bool {
        CharacterSet.whitespacesAndNewlines.contains(scalar)
    }

    /// Bold, italic and strike markers count as word edges, so a mention
    /// wrapped in markdown (as editing a styled message produces) is found.
    private static func opensWord(_ scalar: Unicode.Scalar) -> Bool {
        isSpace(scalar) || "([{\"'*~".unicodeScalars.contains(scalar)
    }

    private static func closesWord(_ scalar: Unicode.Scalar) -> Bool {
        isSpace(scalar) || ",.:;!?)]}\"'*~".unicodeScalars.contains(scalar)
    }

    private static func ownNickBoundary(after index: Int, in scalars: [Unicode.Scalar]) -> Bool {
        guard index < scalars.count else { return true }
        if isSpace(scalars[index]) { return true }
        var boundaryEnd = index
        while boundaryEnd < scalars.count,
              closesWord(scalars[boundaryEnd]),
              !isSpace(scalars[boundaryEnd]) {
            boundaryEnd += 1
        }
        guard boundaryEnd < scalars.count else { return true }
        return isSpace(scalars[boundaryEnd])
    }
}
