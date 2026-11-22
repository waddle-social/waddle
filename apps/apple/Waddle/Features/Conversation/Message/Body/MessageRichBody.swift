import SwiftUI
import WaddleKit

/// The message text: paragraphs with XEP-0394 styling, XEP-0372 mention
/// highlights and detected links; quotes with a leading bar; code blocks
/// in monospace. A XEP-0245 `/me` body renders as an action line,
/// "* Author action". Selectable when `isSelectable`: always on iOS, on a
/// Mac only after "Select text", since there selectable text takes the
/// secondary click for its own Cut/Copy/Paste menu and a right-click on
/// the words would never reach the row's menu.
struct MessageRichBody: View {
    let item: TimelineItem
    let account: AccountIdentity
    /// Compact rows show "(edited)" inline since they have no header.
    let showsEditedMark: Bool
    /// The name the row header shows, used for `/me` action lines.
    let authorName: String
    let isSelectable: Bool
    /// Dynamic Type's factor, for the faces runs pick for themselves.
    @ScaledMetric(relativeTo: .body) private var scale: CGFloat = 1

    var body: some View {
        let hidden = MeAction.hiddenScalarCount(ofBody: item.body)
        let rendered = RichTextLayout.blocks(
            for: MentionHighlight.input(for: item, account: account, hiddenPrefix: hidden),
            detectLinks: MessageLinkDetector.links(in:)
        )
        let blocks = hidden > 0 ? MeActionLine.blocks(rendered, actor: authorName) : rendered
        VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s1Half) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { index, block in
                blockView(block, isLast: index == blocks.count - 1)
            }
        }
        .modifier(MessageTextSelection(isEnabled: isSelectable))
    }

    @ViewBuilder
    private func blockView(_ block: RichBlock, isLast: Bool) -> some View {
        switch block {
        case let .paragraph(segments):
            Text(RichTextAttributes.attributed(segments, editedMark: isLast && showsEditedMark, scale: scale))
                .huddleText(Theme.Typography.message)
                .foregroundStyle(Theme.Colors.fg)
                .fixedSize(horizontal: false, vertical: true)
        case let .quote(segments):
            HStack(alignment: .top, spacing: Theme.Spacing.s) {
                Capsule()
                    .fill(Theme.Colors.border)
                    .frame(width: 3)
                Text(RichTextAttributes.attributed(segments, editedMark: isLast && showsEditedMark, scale: scale))
                    .huddleText(Theme.Typography.message)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .fixedSize(horizontal: false, vertical: true)
        case let .code(code):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .huddleText(Theme.Typography.code)
                    .foregroundStyle(Theme.Colors.fg)
                    .padding(Theme.Spacing.m)
            }
            .background(
                RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous)
                    .fill(Theme.Colors.surfaceCode)
            )
        }
    }
}

/// Maps styled runs to SwiftUI text attributes (web `MessageBody`). Bold,
/// code and mentions pick their own face from the message style at
/// `scale`; the rest inherits it.
enum RichTextAttributes {
    static func attributed(_ segments: [RichSegment], editedMark: Bool, scale: CGFloat = 1) -> AttributedString {
        var result = AttributedString()
        for segment in segments {
            result.append(styled(segment, scale: scale))
        }
        if editedMark {
            var mark = AttributedString(" (edited)")
            mark.foregroundColor = Theme.Colors.fgMuted
            mark.font = Theme.Typography.timestamp.resolved(scale: scale).font
            result.append(mark)
        }
        return result
    }

    private static func styled(_ segment: RichSegment, scale: CGFloat) -> AttributedString {
        var piece = AttributedString(segment.text)
        var intent: InlinePresentationIntent = []
        let bold = Theme.Typography.message.weight(700).resolved(scale: scale).font
        for style in segment.styles {
            switch style {
            case .bold:
                piece.font = bold
            case .italic:
                intent.insert(.emphasized)
            case .strikethrough:
                intent.insert(.strikethrough)
            case .code:
                piece.font = Theme.Typography.code.resolved(scale: scale).font
                piece.backgroundColor = Theme.Colors.surfaceCode
            case let .link(url):
                piece.link = url
                piece.foregroundColor = Theme.Colors.actionText
            case .mention:
                // Every mention alike, as on web; the row marks ours.
                piece.font = bold
                piece.foregroundColor = Theme.Colors.action
                piece.backgroundColor = Theme.Colors.goldSubtle
            }
        }
        if !intent.isEmpty {
            piece.inlinePresentationIntent = intent
        }
        return piece
    }
}

private struct MessageTextSelection: ViewModifier {
    let isEnabled: Bool

    func body(content: Content) -> some View {
        if isEnabled {
            content.textSelection(.enabled)
        } else {
            content.textSelection(.disabled)
        }
    }
}
