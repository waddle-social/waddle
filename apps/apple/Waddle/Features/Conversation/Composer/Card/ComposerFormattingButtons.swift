import SwiftUI
import WaddleKit

struct ComposerFormattingButtons: View {
    var activeFormats: Set<ComposerFormat> = []
    let perform: (ComposerSelectionFormatting.Action) -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(ComposerSelectionFormatting.orderedActions, id: \.self) { action in
                ComposerIconButton(symbol: action.symbol, label: action.title, isOn: action.isActive(in: activeFormats)) {
                    perform(action)
                }
            }
        }
    }
}

struct ComposerSelectionFormatCommands {
    let suppression: ComposerBubbleVisibility.Suppression
    let formatRevision: Int
    let roomLinks: [RoomLinkDraft]
    let perform: (ComposerSelectionFormatting.Action) -> Void
}

extension ComposerSelectionFormatting.Action {
    private var format: ComposerFormat? {
        switch self {
        case let .format(format): return format
        case .link: return nil
        }
    }

    var symbol: String { format?.symbol ?? "link" }
    var title: String { format?.title ?? "Link" }

    func isActive(in formats: Set<ComposerFormat>) -> Bool {
        format.map(formats.contains) ?? false
    }

    func perform(onFormat: (ComposerFormat) -> Void, onLink: () -> Void) {
        if let format { onFormat(format) } else { onLink() }
    }
}

private extension ComposerFormat {
    var symbol: String {
        switch self {
        case .bold: return "bold"
        case .italic: return "italic"
        case .strikethrough: return "strikethrough"
        case .orderedList: return "list.number"
        case .bulletList: return "list.bullet"
        case .code: return "chevron.left.forwardslash.chevron.right"
        case .codeBlock: return "curlybraces"
        case .quote: return "text.quote"
        }
    }

    var title: String {
        switch self {
        case .bold: return "Bold"
        case .italic: return "Italic"
        case .strikethrough: return "Strikethrough"
        case .orderedList: return "Numbered list"
        case .bulletList: return "Bulleted list"
        case .code: return "Code"
        case .codeBlock: return "Code block"
        case .quote: return "Quote"
        }
    }
}
