import Foundation

/// Unicode-scalar helpers shared by the composer edit functions.
enum ComposerScalars {
    /// The selection clamped to `count`; nil becomes a caret at the end.
    static func clamp(_ selection: Range<Int>?, count: Int) -> Range<Int> {
        guard let selection else { return count..<count }
        let lower = min(max(selection.lowerBound, 0), count)
        let upper = min(max(selection.upperBound, lower), count)
        return lower..<upper
    }

    static func string(_ scalars: [Unicode.Scalar]) -> String {
        String(String.UnicodeScalarView(scalars))
    }

    static func isWhitespace(_ scalar: Unicode.Scalar) -> Bool {
        CharacterSet.whitespacesAndNewlines.contains(scalar)
    }
}
