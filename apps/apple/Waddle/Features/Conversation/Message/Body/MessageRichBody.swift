import SwiftUI
import WaddleKit

/// The message text: paragraphs with XEP-0394 styling, XEP-0372 mention
/// highlights and detected links; quotes with a leading bar; code blocks
/// in monospace. Selectable.
struct MessageRichBody: View {
    let item: TimelineItem
    let account: AccountIdentity
    /// Compact rows show "(edited)" inline since they have no header.
    let showsEditedMark: Bool

    var body: some View {
        let blocks = RichTextLayout.blocks(
            for: MentionHighlight.input(for: item, account: account),
            detectLinks: MessageLinkDetector.links(in:)
        )
        VStack(alignment: .leading, spacing: Theme.Spacing.s - 2) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { index, block in
                blockView(block, isLast: index == blocks.count - 1)
            }
        }
        .textSelection(.enabled)
    }

    @ViewBuilder
    private func blockView(_ block: RichBlock, isLast: Bool) -> some View {
        switch block {
        case let .paragraph(segments):
            Text(RichTextAttributes.attributed(segments, editedMark: isLast && showsEditedMark))
                .font(.body)
                .fixedSize(horizontal: false, vertical: true)
        case let .quote(segments):
            HStack(alignment: .top, spacing: Theme.Spacing.s) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(Color.secondary.opacity(0.5))
                    .frame(width: 3)
                Text(RichTextAttributes.attributed(segments, editedMark: isLast && showsEditedMark))
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .fixedSize(horizontal: false, vertical: true)
        case let .code(code):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(.callout, design: .monospaced))
                    .padding(Theme.Spacing.s)
            }
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
                    .fill(Color.secondary.opacity(0.12))
            )
        }
    }
}

/// Maps styled runs to SwiftUI text attributes.
enum RichTextAttributes {
    static func attributed(_ segments: [RichSegment], editedMark: Bool) -> AttributedString {
        var result = AttributedString()
        for segment in segments {
            result.append(styled(segment))
        }
        if editedMark {
            var mark = AttributedString(" (edited)")
            mark.foregroundColor = Color.secondary
            mark.font = Font.caption
            result.append(mark)
        }
        return result
    }

    private static func styled(_ segment: RichSegment) -> AttributedString {
        var piece = AttributedString(segment.text)
        var intent: InlinePresentationIntent = []
        for style in segment.styles {
            switch style {
            case .bold:
                intent.insert(.stronglyEmphasized)
            case .italic:
                intent.insert(.emphasized)
            case .strikethrough:
                intent.insert(.strikethrough)
            case .code:
                intent.insert(.code)
                piece.backgroundColor = Color.secondary.opacity(0.15)
            case let .link(url):
                piece.link = url
            case .mention(.someone):
                intent.insert(.stronglyEmphasized)
                piece.foregroundColor = Color.accentColor
            case .mention(.me):
                intent.insert(.stronglyEmphasized)
                piece.foregroundColor = Color.accentColor
                piece.backgroundColor = Color.accentColor.opacity(0.2)
            }
        }
        if !intent.isEmpty {
            piece.inlinePresentationIntent = intent
        }
        return piece
    }
}
