import Foundation

/// Applies formatting-bar styles to a plain-text draft as the markdown
/// markers `ComposerMarkdown` understands. Selections are Unicode-scalar
/// ranges; a nil selection means the field reports none (before iOS 18 /
/// macOS 15) and the style applies to the whole draft.
public enum ComposerFormatting {
    /// Wraps the selection in the style's markers, keeping the wrapped
    /// text selected. An empty selection inserts an empty marker pair with
    /// the caret between the markers.
    public static func apply(_ format: ComposerFormat, to text: String, selection: Range<Int>?) -> ComposerTextEdit {
        let scalars = Array(text.unicodeScalars)
        let range = ComposerScalars.clamp(selection ?? contentRange(scalars), count: scalars.count)
        switch format {
        case .bold: return wrap(scalars, range, marker: "**")
        case .italic: return wrap(scalars, range, marker: "*")
        case .strikethrough: return wrap(scalars, range, marker: "~~")
        case .code: return wrap(scalars, range, marker: "`")
        case .codeBlock: return fence(scalars, range)
        case .quote: return quote(scalars, range)
        }
    }

    /// The draft without its surrounding whitespace, so markers sit
    /// against text; a blank draft is a caret at its end.
    private static func contentRange(_ scalars: [Unicode.Scalar]) -> Range<Int> {
        let isBlank: (Unicode.Scalar) -> Bool = { $0.properties.isWhitespace }
        guard let first = scalars.firstIndex(where: { !isBlank($0) }),
              let last = scalars.lastIndex(where: { !isBlank($0) })
        else { return scalars.count..<scalars.count }
        return first..<(last + 1)
    }

    private static func wrap(_ scalars: [Unicode.Scalar], _ range: Range<Int>, marker text: String) -> ComposerTextEdit {
        let marker = Array(text.unicodeScalars)
        let result = Array(scalars[..<range.lowerBound]) + marker
            + Array(scalars[range]) + marker
            + Array(scalars[range.upperBound...])
        let shift = marker.count
        return ComposerTextEdit(
            text: ComposerScalars.string(result),
            selection: (range.lowerBound + shift)..<(range.upperBound + shift)
        )
    }

    /// Puts the selection on its own lines between ```` ``` ```` fences.
    private static func fence(_ scalars: [Unicode.Scalar], _ range: Range<Int>) -> ComposerTextEdit {
        let startsLine = range.lowerBound == 0 || scalars[range.lowerBound - 1] == "\n"
        let endsLine = range.upperBound == scalars.count || scalars[range.upperBound] == "\n"
        let opening = Array(((startsLine ? "" : "\n") + "```\n").unicodeScalars)
        let closing = Array(("\n```" + (endsLine ? "" : "\n")).unicodeScalars)
        let selected = Array(scalars[range])
        let result = Array(scalars[..<range.lowerBound]) + opening + selected + closing
            + Array(scalars[range.upperBound...])
        let start = range.lowerBound + opening.count
        return ComposerTextEdit(text: ComposerScalars.string(result), selection: start..<(start + selected.count))
    }

    /// Prefixes every line the selection touches with `> `.
    private static func quote(_ scalars: [Unicode.Scalar], _ range: Range<Int>) -> ComposerTextEdit {
        let starts = lineStarts(scalars, touching: range)
        let marker = Array("> ".unicodeScalars)
        var result = scalars
        for start in starts.reversed() {
            result.insert(contentsOf: marker, at: start)
        }
        let lower = range.lowerBound + marker.count
        let upper = range.upperBound + marker.count * starts.count
        return ComposerTextEdit(text: ComposerScalars.string(result), selection: lower..<upper)
    }

    /// Offsets where each line the range touches begins. A selection that
    /// ends right after a newline does not touch the following line.
    private static func lineStarts(_ scalars: [Unicode.Scalar], touching range: Range<Int>) -> [Int] {
        let first = scalars[..<range.lowerBound].lastIndex(of: "\n").map { $0 + 1 } ?? 0
        let endsAfterNewline = range.upperBound > range.lowerBound && scalars[range.upperBound - 1] == "\n"
        let end = endsAfterNewline ? range.upperBound - 1 : range.upperBound
        let later = (first..<max(first, end)).filter { scalars[$0] == "\n" }.map { $0 + 1 }
        return [first] + later
    }
}
