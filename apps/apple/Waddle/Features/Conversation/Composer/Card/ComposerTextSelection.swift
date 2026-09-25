import SwiftUI

/// Converts between SwiftUI's `TextSelection` and the Unicode-scalar
/// offsets the WaddleKit formatting helpers use. Offsets never hold
/// `String.Index` values across edits, so a selection always resolves
/// against the text it is applied to.
@available(iOS 18.0, macOS 15.0, *)
enum ComposerTextSelection {
    /// The field selection for `range`, clamped to `text`.
    static func selection(for range: Range<Int>?, in text: String) -> TextSelection? {
        guard let range else { return nil }
        let scalars = text.unicodeScalars
        let count = scalars.count
        let lower = scalars.index(scalars.startIndex, offsetBy: min(max(range.lowerBound, 0), count))
        let upper = scalars.index(scalars.startIndex, offsetBy: min(max(range.upperBound, 0), count))
        guard lower < upper else { return TextSelection(insertionPoint: lower) }
        return TextSelection(range: lower..<upper)
    }

    /// Scalar offsets of a single-range selection in `text`; nil for a
    /// multi-range selection. `text` and `selection` are reported by the
    /// field independently and not in a guaranteed order, so an endpoint
    /// from a newer string than `text` holds is clamped to `text`'s end
    /// rather than rejected — otherwise the tracked selection can never
    /// advance past that mismatch, pinning the caret behind where the
    /// field is actually inserting.
    static func range(of selection: TextSelection, in text: String) -> Range<Int>? {
        guard case let .selection(range) = selection.indices else { return nil }
        let count = text.unicodeScalars.count
        let lower = offset(of: range.lowerBound, in: text) ?? count
        let upper = offset(of: range.upperBound, in: text) ?? count
        return min(lower, upper)..<max(lower, upper)
    }

    private static func offset(of index: String.Index, in text: String) -> Int? {
        guard index >= text.startIndex, index <= text.endIndex else { return nil }
        let scalars = text.unicodeScalars
        guard let aligned = index.samePosition(in: scalars) else { return nil }
        return scalars.distance(from: scalars.startIndex, to: aligned)
    }
}

/// Whether the text field reports its selection on this system. Where it
/// does not, the composer keeps `selection` nil so edits apply at the end
/// of the draft instead of at an offset the field never reported.
enum ComposerSelectionSupport {
    static var isAvailable: Bool {
        if #available(iOS 18.0, macOS 15.0, *) {
            return true
        } else {
            return false
        }
    }
}
