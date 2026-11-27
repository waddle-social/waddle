import SwiftUI
import WaddleKit

/// The `Aa` bar: text styles, list prefixes and a bare URL.
struct ComposerFormattingBar: View {
    let onFormat: (ComposerFormat) -> Void
    let onLink: () -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            ComposerFormattingButtons { action in
                action.perform(onFormat: onFormat, onLink: onLink)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Formatting"))
    }
}
