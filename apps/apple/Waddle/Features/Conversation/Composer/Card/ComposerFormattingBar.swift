import SwiftUI
import WaddleKit

/// The `Aa` bar: markdown styles `ComposerMarkdown` converts at send
/// time, plus a link inserted as a bare URL.
struct ComposerFormattingBar: View {
    /// False when styles have nothing to wrap: an empty draft in a field
    /// that reports no caret (before iOS 18 / macOS 15).
    let canFormat: Bool
    let onFormat: (ComposerFormat) -> Void
    let onLink: () -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 0) {
                ForEach(ComposerFormat.allCases, id: \.self) { format in
                    ComposerIconButton(symbol: format.symbol, label: format.title) {
                        onFormat(format)
                    }
                    .disabled(!canFormat)
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
