import Foundation
import WaddleKit

/// Turns a row's displayed body and XEP-0394 spans back into the markdown
/// the composer understands, so editing a formatted message keeps its
/// formatting. Blockquote markers already live in the body; links and
/// mentions are plain text in markdown.
enum EditableMarkdown {
    static func text(for item: TimelineItem) -> String {
        let scalars = Array(item.body.unicodeScalars)
        let mapping: WireBodyMapping? = item.isEdited
            ? WireBodyMapping(correctedBody: item.body, fallback: item.message.reply?.fallback)
            : item.message.body.flatMap {
                WireBodyMapping(wireBody: $0, fallback: item.message.reply?.fallback, displayedBody: item.body)
            }
        guard let mapping else { return item.body }
        var inserts: [Insert] = []
        for (order, span) in item.message.markupSpans.enumerated() {
            guard let markers = markers(for: span.kind),
                  let range = mapping.displayedRange(ofWire: span.start, span.end)
            else { continue }
            inserts.append(Insert(offset: range.lowerBound, text: markers.open, isClose: false, order: order))
            inserts.append(Insert(offset: range.upperBound, text: markers.close, isClose: true, order: order))
        }
        guard !inserts.isEmpty else { return item.body }
        return apply(inserts.sorted(by: Insert.precedes), to: scalars)
    }

    private struct Insert {
        let offset: Int
        let text: String
        let isClose: Bool
        let order: Int

        /// By offset; at one offset, closes (innermost first) before opens.
        static func precedes(_ lhs: Insert, _ rhs: Insert) -> Bool {
            if lhs.offset != rhs.offset { return lhs.offset < rhs.offset }
            if lhs.isClose != rhs.isClose { return lhs.isClose }
            return lhs.isClose ? lhs.order > rhs.order : lhs.order < rhs.order
        }
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

    private static func apply(_ inserts: [Insert], to scalars: [Unicode.Scalar]) -> String {
        var result = String.UnicodeScalarView()
        var cursor = 0
        for insert in inserts {
            let offset = min(max(insert.offset, cursor), scalars.count)
            result.append(contentsOf: scalars[cursor..<offset])
            result.append(contentsOf: insert.text.unicodeScalars)
            cursor = offset
        }
        result.append(contentsOf: scalars[cursor...])
        return String(result)
    }
}
