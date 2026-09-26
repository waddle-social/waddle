import Foundation

/// Converts between a text view's UTF-16 selection and the Unicode scalar
/// offsets the WaddleKit formatting helpers use. Offsets never hold
/// `String.Index` values across edits, so a selection always resolves
/// against the text it is applied to.
enum ComposerUTF16Selection {
    /// Scalar offsets of `range` in `text`; nil when `range` does not fall
    /// on scalar boundaries of `text`.
    static func scalarRange(of range: NSRange, in text: String) -> Range<Int>? {
        guard range.location != NSNotFound, let bounds = Range(range, in: text) else { return nil }
        let scalars = text.unicodeScalars
        guard let lower = bounds.lowerBound.samePosition(in: scalars),
              let upper = bounds.upperBound.samePosition(in: scalars)
        else { return nil }
        let start = scalars.distance(from: scalars.startIndex, to: lower)
        let end = scalars.distance(from: scalars.startIndex, to: upper)
        return start..<end
    }

    /// The UTF-16 range of scalar offsets `range`, clamped to `text`.
    static func nsRange(for range: Range<Int>, in text: String) -> NSRange {
        let scalars = text.unicodeScalars
        let count = scalars.count
        let lower = scalars.index(scalars.startIndex, offsetBy: min(max(range.lowerBound, 0), count))
        let upper = scalars.index(scalars.startIndex, offsetBy: min(max(range.upperBound, 0), count))
        return NSRange(lower..<upper, in: text)
    }
}
