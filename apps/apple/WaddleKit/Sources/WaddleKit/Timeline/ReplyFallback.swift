import Foundation

/// XEP-0428 fallback handling for XEP-0461 replies. Offsets count Unicode
/// scalar values; `end` is exclusive.
public enum ReplyFallback {
    /// Removes the fallback range from `body`. Out-of-range values are
    /// clamped so a malformed range never crashes rendering.
    public static func strip(_ body: String, range: Range<Int>?) -> String {
        guard let range, range.lowerBound < range.upperBound else { return body }
        let scalars = Array(body.unicodeScalars)
        let start = max(0, min(range.lowerBound, scalars.count))
        let end = max(start, min(range.upperBound, scalars.count))
        guard start < end else { return body }
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars[..<start])
        view.append(contentsOf: scalars[end...])
        return String(view).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Builds the quote prefix a reply carries for clients without
    /// XEP-0461 support, and its scalar range. Same shape as the web and
    /// Android clients: every parent line prefixed `> `, then a blank line.
    /// An empty parent (attachment-only) gets no quote, since a bare `> `
    /// renders as a stray line elsewhere.
    public static func quote(parentBody: String) -> (prefix: String, range: Range<Int>)? {
        guard !parentBody.isEmpty else { return nil }
        let prefix = parentBody
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { "> \($0)" }
            .joined(separator: "\n") + "\n\n"
        return (prefix, 0..<prefix.unicodeScalars.count)
    }
}
