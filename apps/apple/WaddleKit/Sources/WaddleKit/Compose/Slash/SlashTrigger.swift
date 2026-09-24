import Foundation

/// A `/command` being typed at the start of the draft.
///
/// Mirrors the web composer's trigger: the draft must start with `/`,
/// optionally followed by a command word (`[a-zA-Z][a-zA-Z0-9_-]*`), and
/// then either nothing or whitespace plus any trailing text (which may span
/// lines). Anything else (`/1`, `/foo!`, ` /me`) is not a trigger.
public struct SlashTrigger: Hashable, Sendable {
    /// The command word after `/`; empty for a bare `/` or `/ text`.
    public let prefix: String
    /// Everything after the first whitespace, leading whitespace trimmed.
    public let trailing: String

    public init(prefix: String, trailing: String) {
        self.prefix = prefix
        self.trailing = trailing
    }

    public static func parse(_ text: String) -> SlashTrigger? {
        let scalars = text.unicodeScalars
        guard scalars.first == "/" else { return nil }
        let rest = scalars.dropFirst()
        let word = rest.prefix(while: isWordScalar)
        if let first = word.first, !isASCIILetter(first) { return nil }
        let afterWord = rest.dropFirst(word.count)
        guard let separator = afterWord.first else {
            return SlashTrigger(prefix: string(word), trailing: "")
        }
        guard isWhitespace(separator) else { return nil }
        let trailing = afterWord.dropFirst().drop(while: isWhitespace)
        return SlashTrigger(prefix: string(word), trailing: string(trailing))
    }

    private static func isASCIILetter(_ scalar: Unicode.Scalar) -> Bool {
        ("a"..."z").contains(scalar) || ("A"..."Z").contains(scalar)
    }

    private static func isWordScalar(_ scalar: Unicode.Scalar) -> Bool {
        isASCIILetter(scalar) || ("0"..."9").contains(scalar) || scalar == "_" || scalar == "-"
    }

    private static func isWhitespace(_ scalar: Unicode.Scalar) -> Bool {
        CharacterSet.whitespacesAndNewlines.contains(scalar)
    }

    private static func string(_ scalars: Substring.UnicodeScalarView) -> String {
        String(Substring(scalars))
    }
}
