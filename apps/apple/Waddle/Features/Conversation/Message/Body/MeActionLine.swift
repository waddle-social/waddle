import Foundation

/// A XEP-0245 action line, "* Author action": the whole line in italics
/// with the author in bold. The action keeps its own markup, mentions and
/// links; code blocks stay verbatim.
enum MeActionLine {
    /// `action` is the body rendered with its "/me " prefix hidden.
    static func blocks(_ action: [RichBlock], actor: String) -> [RichBlock] {
        let italic = action.map(italicizedBlock)
        let lead: [RichSegment] = [
            RichSegment(text: "* ", styles: [.italic]),
            RichSegment(text: actor, styles: [.bold, .italic]),
        ]
        guard case let .paragraph(first)? = italic.first else {
            return [RichBlock.paragraph(lead)] + italic
        }
        let line = lead + [RichSegment(text: " ", styles: [.italic])] + first
        return [RichBlock.paragraph(line)] + Array(italic.dropFirst())
    }

    private static func italicizedBlock(_ block: RichBlock) -> RichBlock {
        switch block {
        case let .paragraph(segments):
            return .paragraph(segments.map(italicizedSegment))
        case let .quote(segments):
            return .quote(segments.map(italicizedSegment))
        case .code:
            return block
        }
    }

    private static func italicizedSegment(_ segment: RichSegment) -> RichSegment {
        RichSegment(text: segment.text, styles: segment.styles.union([.italic]))
    }
}
