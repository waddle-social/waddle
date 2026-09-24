import Foundation

/// Where a XEP-0245 `/me` action starts in its body. XEP-0394 markup and
/// XEP-0372 reference offsets count Unicode scalars over the full body,
/// prefix included, so an action line keeps those offsets and only hides
/// the prefix scalars.
extension MeAction {
    /// Unicode scalars in "/me ".
    public static let prefixScalarCount = prefix.unicodeScalars.count

    /// Scalars an action line hides at the start of `body`: the prefix for
    /// a `/me` body, else none.
    public static func hiddenScalarCount(ofBody body: String) -> Int {
        parse(body: body) == nil ? 0 : prefixScalarCount
    }

    /// `range` (scalar offsets over the full body) without its first
    /// `hidden` scalars, still in full-body offsets. A range that starts
    /// inside the hidden prefix is clamped to the prefix end; nil when
    /// nothing visible remains.
    public static func visibleRange(_ range: Range<Int>, hiding hidden: Int) -> Range<Int>? {
        let lower = max(range.lowerBound, max(hidden, 0))
        return lower < range.upperBound ? lower..<range.upperBound : nil
    }
}
