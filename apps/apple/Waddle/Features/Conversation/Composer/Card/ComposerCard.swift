import SwiftUI
import WaddleKit

/// The Slack-style card: optional formatting bar, the text field, pending
/// attachments, and the action row.
struct ComposerCard: View {
    let model: ComposerModel
    /// The draft text; the composer notes what the field writes.
    @Binding var text: String
    @Binding var selection: Range<Int>?
    @Binding var showsFormatting: Bool
    var isFocused: FocusState<Bool>.Binding
    let placeholder: String
    let showsMention: Bool
    let uploader: AttachmentUploader
    let actions: ComposerActions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if showsFormatting {
                ComposerFormattingBar(
                    canFormat: ComposerSelectionSupport.isAvailable || !text.isEmpty,
                    onFormat: actions.format,
                    onLink: actions.requestLink
                )
                    .padding(.horizontal, Theme.Spacing.xs)
                Divider()
                    .padding(.horizontal, Theme.Spacing.s)
            }
            ComposerTextField(
                text: $text,
                selection: $selection,
                placeholder: model.isEditing ? "Edit message" : placeholder,
                isFocused: isFocused,
                onSubmit: actions.submit,
                onAcceptSuggestion: actions.acceptSuggestion,
                onCancel: actions.cancelContext,
                onPaste: actions.paste
            )
            .padding(.horizontal, Theme.Spacing.m)
            .padding(.top, Theme.Spacing.xs)
            if !model.attachments.isEmpty, !model.isEditing {
                ComposerAttachmentStrip(model: model, uploader: uploader)
                    .padding(.horizontal, Theme.Spacing.m)
            }
            ComposerActionRow(
                isEditing: model.isEditing,
                canSend: model.canSend,
                showsMention: showsMention,
                showsFormatting: $showsFormatting,
                actions: actions
            )
            .padding(.horizontal, Theme.Spacing.xs)
            .padding(.bottom, Theme.Spacing.xxs)
        }
        .background(cardBackground)
        .overlay(
            RoundedRectangle(cornerRadius: ComposerMetrics.cardRadius, style: .continuous)
                .strokeBorder(borderColor, lineWidth: 1)
        )
        .animation(.easeInOut(duration: 0.18), value: isFocused.wrappedValue)
    }

    @ViewBuilder
    private var cardBackground: some View {
        #if os(iOS)
        RoundedRectangle(cornerRadius: ComposerMetrics.cardRadius, style: .continuous)
            .fill(.regularMaterial)
        #else
        RoundedRectangle(cornerRadius: ComposerMetrics.cardRadius, style: .continuous)
            .fill(Color.secondaryBackground)
        #endif
    }

    private var borderColor: Color {
        #if os(iOS)
        isFocused.wrappedValue ? Color.accentColor.opacity(0.45) : Color.primary.opacity(0.08)
        #else
        isFocused.wrappedValue ? Color.accentColor.opacity(0.5) : Color.secondary.opacity(0.2)
        #endif
    }
}
