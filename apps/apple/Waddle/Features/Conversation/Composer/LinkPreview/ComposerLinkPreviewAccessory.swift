import SwiftUI
import WaddleKit

struct ComposerLinkPreviewAccessory: View {
    let model: ComposerModel

    var body: some View {
        if model.linkPreview.state != .idle {
            ComposerLinkPreviewCard(state: model.linkPreview.state, dismiss: model.linkPreview.dismiss)
        }
    }
}

/// Observe the composer even before any card is visible.
struct ComposerLinkPreviewUpdates: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    let model: ComposerModel
    let conversation: ConversationID

    func body(content: Content) -> some View {
        content
            .disabled(model.isDraftLocked)
            .onChange(of: input, initial: true) {
                model.updateLinkPreview(in: conversation, session: session)
            }
            .onDisappear {
                if model.isEditing, model.linkPreviews.isPreparing { model.linkPreviews.resetEdit() }
            }
    }

    private var input: Input {
        Input(
            text: model.text, conversation: conversation,
            editRevision: model.linkPreviews.editRevision, isEditing: model.isEditing,
            session: ObjectIdentifier(session)
        )
    }

    private struct Input: Equatable {
        let text: String
        let conversation: ConversationID
        let editRevision: Int
        let isEditing: Bool
        let session: ObjectIdentifier
    }
}
