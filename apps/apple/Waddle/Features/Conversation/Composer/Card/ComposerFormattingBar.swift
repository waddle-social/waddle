import SwiftUI
import WaddleKit

/// The `Aa` bar: markdown styles `ComposerMarkdown` converts at send
/// time, plus a link inserted as a bare URL.
struct ComposerFormattingBar: View {
    let onFormat: (ComposerFormat) -> Void
    let onLink: () -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 0) {
                ForEach(ComposerFormat.allCases, id: \.self) { format in
                    ComposerIconButton(symbol: format.symbol, label: format.title) {
                        onFormat(format)
                    }
                }
                ComposerIconButton(symbol: "link", label: "Link", action: onLink)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Formatting"))
    }
}

private extension ComposerFormat {
    var symbol: String {
        switch self {
        case .bold: return "bold"
        case .italic: return "italic"
        case .strikethrough: return "strikethrough"
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
        case .code: return "Code"
        case .codeBlock: return "Code block"
        case .quote: return "Quote"
        }
    }
}
