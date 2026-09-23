import Foundation

/// The markdown markers that re-create a row's XEP-0394 spans in the
/// composer, placed over the displayed body. Blockquote markers already
/// live in the body and links are plain text in markdown, so neither
/// inserts anything.
struct EditableMarkup {
    struct Insert {
        /// Scalar offset over the displayed body.
        let offset: Int
        let text: [Unicode.Scalar]
        let isClose: Bool
        let order: Int

        /// By offset; at one offset, closes (innermost first) before opens.
        static func precedes(_ lhs: Insert, _ rhs: Insert) -> Bool {
            if lhs.offset != rhs.offset { return lhs.offset < rhs.offset }
            if lhs.isClose != rhs.isClose { return lhs.isClose }
            return lhs.isClose ? lhs.order > rhs.order : lhs.order < rhs.order
        }
    }

    /// Application order; offsets lie within the displayed body.
    let inserts: [Insert]

    init(spans: [MarkupSpan], mapping: WireBodyMapping, displayedLength: Int) {
        var inserts: [Insert] = []
        for (order, span) in spans.enumerated() {
            guard let markers = Self.markers(for: span.kind),
                  let range = mapping.displayedRange(ofWire: span.start, span.end)
            else { continue }
            let lower = min(range.lowerBound, displayedLength)
            let upper = min(range.upperBound, displayedLength)
            inserts.append(Insert(offset: lower, text: Array(markers.open.unicodeScalars), isClose: false, order: order))
            inserts.append(Insert(offset: upper, text: Array(markers.close.unicodeScalars), isClose: true, order: order))
        }
        self.inserts = inserts.sorted(by: Insert.precedes)
    }

    /// The displayed body with every marker inserted.
    func apply(to scalars: [Unicode.Scalar]) -> String {
        var result = String.UnicodeScalarView()
        var cursor = 0
        for insert in inserts {
            result.append(contentsOf: scalars[cursor..<insert.offset])
            result.append(contentsOf: insert.text)
            cursor = insert.offset
        }
        result.append(contentsOf: scalars[cursor...])
        return String(result)
    }

    /// Where a displayed range lands in the marked-up text: after the
    /// markers at its start, before the markers at its end. Nil when a
    /// marker falls strictly inside, since that text no longer reads the
    /// same.
    func editableRange(ofDisplayed range: Range<Int>) -> Range<Int>? {
        guard !inserts.contains(where: { range.lowerBound < $0.offset && $0.offset < range.upperBound }) else {
            return nil
        }
        let lower = range.lowerBound + markerLength { $0.offset <= range.lowerBound }
        let upper = range.upperBound + markerLength { $0.offset < range.upperBound }
        return lower..<upper
    }

    private func markerLength(where included: (Insert) -> Bool) -> Int {
        inserts.filter(included).reduce(0) { $0 + $1.text.count }
    }

    private static func markers(for kind: MarkupSpan.Kind) -> (open: String, close: String)? {
        switch kind {
        case .bold: return ("**", "**")
        case .italic: return ("*", "*")
        case .strikethrough: return ("~~", "~~")
        case .code: return ("`", "`")
        case .codeBlock: return ("```\n", "\n```")
        case .blockquote, .link: return nil
        }
    }
}
