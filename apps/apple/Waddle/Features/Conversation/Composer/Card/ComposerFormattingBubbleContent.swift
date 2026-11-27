#if os(macOS)
import SwiftUI
import WaddleKit

struct ComposerFormattingBubbleContent: View {
    let formatting: ComposerSelectionFormatting
    let perform: (ComposerSelectionFormatting.Action) -> Void

    var body: some View {
        ComposerFormattingButtons(activeFormats: formatting.activeFormats, perform: perform)
            .padding(Theme.Spacing.xxs)
            .huddleSurface(.raised, radius: Theme.Radius.medium)
            .fixedSize()
    }
}
#endif
