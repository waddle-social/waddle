import SwiftUI
import WaddleKit

/// The composer panel (web `wd-composer`): the formatting bar and
/// pending attachments when there are any, then one row of `+`, the text,
/// the other buttons (iPad, Mac) and Send.
struct ComposerCard: View {
    @Environment(\.horizontalSizeClass) private var sizeClass
    let model: ComposerModel
    /// The draft text; the composer notes what the field writes.
    @Binding var text: String
    @Binding var selection: Range<Int>?
    @Binding var showsFormatting: Bool
    @Binding var isFocused: Bool
    let placeholder: String
    let showsMention: Bool
    let uploader: AttachmentUploader
    let actions: ComposerActions

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: ComposerMetrics.cardRadius, style: .continuous)
        VStack(alignment: .leading, spacing: 0) {
            if showsFormatting {
                ComposerFormattingBar(
                    onFormat: actions.format,
                    onLink: actions.requestLink
                )
                HuddleDivider()
                    .padding(.horizontal, Theme.Spacing.xs)
                    .padding(.bottom, Theme.Spacing.xs)
            }
            if !model.attachments.isEmpty, !model.isEditing {
                ComposerAttachmentStrip(model: model, uploader: uploader)
                    .padding(.horizontal, Theme.Spacing.xs)
            }
            HStack(alignment: .bottom, spacing: Theme.Spacing.xs) {
                ComposerPlusMenu(
                    isEditing: model.isEditing,
                    showsMention: showsMention,
                    showsFormatting: $showsFormatting,
                    holdsExtraButtons: isCompact,
                    actions: actions
                )
                ComposerTextField(
                    text: $text,
                    selection: $selection,
                    placeholder: model.isEditing ? "Edit message" : placeholder,
                    isFocused: $isFocused,
                    onSubmit: actions.submit,
                    onAcceptSuggestion: actions.acceptSuggestion,
                    onCancel: actions.cancelContext,
                    onPaste: actions.paste
                )
                .padding(.horizontal, Theme.Spacing.s)
                .frame(minHeight: ComposerMetrics.button)
                if !isCompact {
                    ComposerExtraButtons(
                        isEditing: model.isEditing,
                        showsMention: showsMention,
                        showsFormatting: $showsFormatting,
                        actions: actions
                    )
                }
                ComposerSendButton(isEditing: model.isEditing, isEnabled: model.canSend, action: actions.submit)
            }
        }
        .padding(ComposerMetrics.inset)
        .huddleSurface(.card, in: shape)
        // An input, so its edge meets 3:1 contrast; focus adds the ring.
        .overlay(shape.strokeBorder(Theme.Colors.borderStrong, lineWidth: WaddleTokens.BorderWidth.hair))
        .overlay(
            shape
                .inset(by: -WaddleTokens.BorderWidth.focus / 2 - 1)
                .stroke(Theme.Colors.focus, lineWidth: WaddleTokens.BorderWidth.focus)
                .opacity(isFocused ? 1 : 0)
        )
        .huddleAnimation(.settle, value: isFocused)
    }

    /// A phone, or an iPad in a narrow column: the extra buttons move into
    /// the `+` menu.
    private var isCompact: Bool {
        #if os(iOS)
        sizeClass == .compact
        #else
        false
        #endif
    }
}
