import Foundation
import WaddleKit

/// Maps XEP-0394/XEP-0372 offsets, which count Unicode scalars over the
/// wire body, onto the displayed body: the wire body with the XEP-0428
/// reply fallback removed and, when one was removed, trimmed
/// (`ReplyFallback.strip`).
struct WireBodyMapping: Equatable {
    /// Removed fallback scalars over the wire body; empty when none.
    let removed: Range<Int>
    /// Whitespace scalars trimmed from the front after the removal.
    let leadingTrim: Int
    /// Scalar count of the displayed body.
    let displayedLength: Int

    /// Nil when `displayedBody` is not derived from `wireBody` (a XEP-0308
    /// correction replaced it), so the wire offsets no longer apply.
    init?(wireBody: String, fallback: Range<Int>?, displayedBody: String) {
        guard ReplyFallback.strip(wireBody, range: fallback) == displayedBody else { return nil }
        let scalars = Array(wireBody.unicodeScalars)
        let removed = Self.clamped(fallback, count: scalars.count)
        let remainderCount = scalars.count - removed.count
        func scalar(at index: Int) -> Unicode.Scalar {
            index < removed.lowerBound ? scalars[index] : scalars[index + removed.count]
        }
        var leading = 0
        var trailing = 0
        if !removed.isEmpty {
            while leading < remainderCount, Self.isTrimmed(scalar(at: leading)) {
                leading += 1
            }
            while trailing < remainderCount - leading, Self.isTrimmed(scalar(at: remainderCount - 1 - trailing)) {
                trailing += 1
            }
        }
        self.removed = removed
        self.leadingTrim = leading
        self.displayedLength = remainderCount - leading - trailing
    }

    /// A XEP-0308 correction keeps only its display body and fallback
    /// range (the original's wire body stays on the row). Its wire body
    /// was the fallback followed by the body, which the composer trims, so
    /// offsets past the fallback shift by the fallback length.
    init(correctedBody: String, fallback: Range<Int>?) {
        if let fallback, fallback.lowerBound >= 0, fallback.lowerBound < fallback.upperBound {
            removed = fallback
        } else {
            removed = 0..<0
        }
        leadingTrim = 0
        displayedLength = correctedBody.unicodeScalars.count
    }

    /// The displayed range for a wire range. Ranges inside the fallback
    /// are dropped; ranges crossing its edge are clipped to what remains.
    func displayedRange(ofWire lower: Int, _ upper: Int) -> Range<Int>? {
        guard lower < upper else { return nil }
        if !removed.isEmpty, lower >= removed.lowerBound, upper <= removed.upperBound {
            return nil
        }
        let start = displayedOffset(ofWire: lower)
        let end = displayedOffset(ofWire: upper)
        return start < end ? start..<end : nil
    }

    private func displayedOffset(ofWire offset: Int) -> Int {
        let unshifted: Int
        if offset <= removed.lowerBound {
            unshifted = offset
        } else if offset >= removed.upperBound {
            unshifted = offset - removed.count
        } else {
            unshifted = removed.lowerBound
        }
        return min(max(unshifted - leadingTrim, 0), displayedLength)
    }

    /// Same clamping as `ReplyFallback.strip`.
    private static func clamped(_ range: Range<Int>?, count: Int) -> Range<Int> {
        guard let range, range.lowerBound < range.upperBound else { return 0..<0 }
        let start = max(0, min(range.lowerBound, count))
        let end = max(start, min(range.upperBound, count))
        return start < end ? start..<end : 0..<0
    }

    private static func isTrimmed(_ scalar: Unicode.Scalar) -> Bool {
        CharacterSet.whitespacesAndNewlines.contains(scalar)
    }
}
