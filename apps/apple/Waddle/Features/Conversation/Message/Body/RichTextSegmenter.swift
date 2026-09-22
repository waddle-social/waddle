import Foundation

/// Cuts scalar text into runs whose style set is constant.
enum RichTextSegmenter {
    static func segments(_ scalars: [Unicode.Scalar], styles: [RichStyledRange]) -> [RichSegment] {
        guard !scalars.isEmpty else { return [] }
        let count = scalars.count
        let clampedStyles = styles.compactMap { styled -> RichStyledRange? in
            let lower = max(0, styled.range.lowerBound)
            let upper = min(count, styled.range.upperBound)
            return lower < upper ? RichStyledRange(style: styled.style, range: lower..<upper) : nil
        }
        var cuts = Set([0, count])
        for styled in clampedStyles {
            cuts.insert(styled.range.lowerBound)
            cuts.insert(styled.range.upperBound)
        }
        let bounds = cuts.sorted()
        var segments: [RichSegment] = []
        for (lower, upper) in zip(bounds, bounds.dropFirst()) {
            let active = Set(clampedStyles.filter { $0.range.lowerBound <= lower && $0.range.upperBound >= upper }.map(\.style))
            let text = string(scalars[lower..<upper])
            if let last = segments.last, last.styles == active {
                segments[segments.count - 1] = RichSegment(text: last.text + text, styles: active)
            } else {
                segments.append(RichSegment(text: text, styles: active))
            }
        }
        return segments
    }

    static func string(_ scalars: ArraySlice<Unicode.Scalar>) -> String {
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars)
        return String(view)
    }
}
