import Foundation

/// Inserting emoji, mention triggers and links into a plain-text draft.
/// Selections are Unicode-scalar ranges; nil means the end of the draft.
public enum ComposerInsertion {
    /// Replaces the selection with `insertion`, caret after it.
    public static func replacingSelection(with insertion: String, in text: String, selection: Range<Int>?) -> ComposerTextEdit {
        let scalars = Array(text.unicodeScalars)
        let range = ComposerScalars.clamp(selection, count: scalars.count)
        let inserted = Array(insertion.unicodeScalars)
        let result = Array(scalars[..<range.lowerBound]) + inserted + Array(scalars[range.upperBound...])
        let caret = range.lowerBound + inserted.count
        return ComposerTextEdit(text: ComposerScalars.string(result), selection: caret..<caret)
    }

    /// Appends `token` (such as `@`) at the end of the draft, after a space
    /// unless the draft is empty or already ends in whitespace.
    public static func appendingToken(_ token: String, to text: String) -> String {
        guard let last = text.unicodeScalars.last, !ComposerScalars.isWhitespace(last) else {
            return text + token
        }
        return text + " " + token
    }

    /// Inserts `url` after the selection, separated from the surrounding
    /// text by spaces so receivers auto-link it.
    public static func insertingLink(_ url: URL, into text: String, selection: Range<Int>?) -> ComposerTextEdit {
        let scalars = Array(text.unicodeScalars)
        let point = ComposerScalars.clamp(selection, count: scalars.count).upperBound
        let needsLeading = point > 0 && !ComposerScalars.isWhitespace(scalars[point - 1])
        let needsTrailing = point == scalars.count || !ComposerScalars.isWhitespace(scalars[point])
        let insertion = (needsLeading ? " " : "") + url.absoluteString + (needsTrailing ? " " : "")
        return replacingSelection(with: insertion, in: text, selection: point..<point)
    }

    /// The typed link as an absolute http(s) URL, assuming `https://` when
    /// no scheme was typed; nil for anything else.
    public static func linkURL(from input: String) -> URL? {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !trimmed.unicodeScalars.contains(where: ComposerScalars.isWhitespace) else { return nil }
        let candidate = trimmed.contains("://") ? trimmed : "https://" + trimmed
        guard let url = URL(string: candidate),
              let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = url.host, !host.isEmpty
        else { return nil }
        return url
    }
}
