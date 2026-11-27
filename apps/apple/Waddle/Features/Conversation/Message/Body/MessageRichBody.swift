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
    @Environment(\.openURL) private var openURL
    let presentation: MessageBodyPresentation
    /// Compact rows show "(edited)" inline since they have no header.
    let showsEditedMark: Bool
    let isSelectable: Bool
    let openRoom: (BareJID) -> Void
    /// Dynamic Type's factor, for the faces runs pick for themselves.
    @ScaledMetric(relativeTo: .body) private var scale: CGFloat = 1

    var body: some View {
        let blocks = presentation.blocks
        VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s1Half) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { index, block in
                blockView(block, isLast: index == blocks.count - 1)
            }
        }
        .modifier(MessageTextSelection(isEnabled: isSelectable))
        .environment(\.openURL, OpenURLAction { url in
            switch presentation.roomLinks.destination(for: url) {
            case let .room(room): openRoom(room)
            case .external: openURL(url)
            case .discard: return .discarded
            }
            return .handled
        })
    }

    @ViewBuilder
    private func blockView(_ block: RichBlock, isLast: Bool) -> some View {
        switch block {
        case let .paragraph(segments):
            Text(attributed(segments, isLast: isLast))
                .huddleText(Theme.Typography.message)
                .foregroundStyle(Theme.Colors.fg)
                .fixedSize(horizontal: false, vertical: true)
        case let .quote(segments):
            HStack(alignment: .top, spacing: Theme.Spacing.s) {
                Capsule()
                    .fill(Theme.Colors.border)
                    .frame(width: Theme.Size.rail)
                Text(attributed(segments, isLast: isLast))
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

    private func attributed(_ segments: [RichSegment], isLast: Bool) -> AttributedString {
        RichTextAttributes.attributed(
            segments, editedMark: isLast && showsEditedMark, scale: scale, roomURL: presentation.roomLinks.url(for:)
        )
    }
}

/// Maps styled runs to SwiftUI text attributes (web `MessageBody`). Bold,
/// code and mentions pick their own face from the message style at
/// `scale`; the rest inherits it.
enum RichTextAttributes {
    static func attributed(
        _ segments: [RichSegment], editedMark: Bool, scale: CGFloat = 1, roomURL: (BareJID) -> URL?
    ) -> AttributedString {
        var result = AttributedString()
        for segment in segments {
            result.append(styled(segment, scale: scale, roomURL: roomURL))
        }
        if editedMark {
            var mark = AttributedString(" (edited)")
            mark.foregroundColor = Theme.Colors.fgMuted
            mark.font = Theme.Typography.timestamp.resolved(scale: scale).font
            result.append(mark)
        }
        return result
    }

    private static func styled(_ segment: RichSegment, scale: CGFloat, roomURL: (BareJID) -> URL?) -> AttributedString {
        var piece = AttributedString(segment.text)
        var intent: InlinePresentationIntent = []
        var isBold = false
        var isCode = false
        for style in segment.styles {
            switch style {
            case .bold:
                isBold = true
            case .italic:
                intent.insert(.emphasized)
            case .strikethrough:
                intent.insert(.strikethrough)
            case .code:
                isCode = true
                piece.backgroundColor = Theme.Colors.surfaceCode
            case let .link(url):
                piece.link = url
                piece.foregroundColor = Theme.Colors.actionText
            case let .roomLink(room):
                if let url = roomURL(room) {
                    piece.link = url
                    piece.foregroundColor = Theme.Colors.actionText
                }
            case .mention:
                // Every mention alike, as on web; the row marks ours. The
                // text takes the action colour, as in the Penpot frames.
                isBold = true
                piece.foregroundColor = Theme.Colors.action
                piece.backgroundColor = Theme.Colors.goldSubtle
            }
        }
        // One face for the run, whatever order its styles came in.
        if isCode {
            piece.font = Theme.Typography.code.weight(isBold ? 700 : 400).resolved(scale: scale).font
        } else if isBold {
            piece.font = Theme.Typography.message.weight(700).resolved(scale: scale).font
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
