import Foundation
import WaddleKit

extension ComposerModel {
    var linkPreview: ComposerLinkPreview { linkPreviews.preview(isEditing: isEditing) }
    var isDraftLocked: Bool { linkPreviews.isPreparing && !isEditing }

    func updateLinkPreview(in conversation: ConversationID, session: SessionCoordinator) {
        linkPreviews.update(text: text, isEditing: isEditing, in: conversation, session: session)
    }

    func submissionDraft(thread: String?) -> Draft {
        let located = references.located(in: text)
        if isEditing { return Draft(text: text, references: located) }
        return Draft(
            text: text, references: located, reply: reply, thread: thread,
            attachments: attachments.compactMap(\.sharedFile)
        )
    }

    func submitDraft(
        thread: String?, session: SessionCoordinator, in conversation: ConversationID,
        dispatch: @MainActor (ComposerSubmission, LinkPreviewPayload?, @escaping @MainActor @Sendable () -> Void) async -> Void
    ) async {
        guard canSend else { return }
        let draft = submissionDraft(thread: thread)
        let editing = self.editing
        let attachmentIDs = attachments.map(\.id)
        await linkPreviews.submit(
            draft, editing: editing, in: conversation, session: session,
            currentDraft: { self.submissionDraft(thread: thread) },
            consumeNewDraft: { consumesText in
                if consumesText {
                    self.clearText()
                } else if self.text.hasPrefix(draft.text) {
                    // Typed on while the preview was awaited: what was sent goes, what was added stays.
                    let rest = String(self.text.dropFirst(draft.text.count))
                    self.replaceText(ComposerTextReplacement(rest, replacing: 0..<draft.text.unicodeScalars.count))
                }
                if self.reply == draft.reply { self.cancelReply() }
                for id in attachmentIDs { self.removeAttachment(id) }
            },
            perform: { frozen, payload, onQueued in
                let submission: ComposerSubmission
                if editing != nil {
                    guard let edit = self.takeSubmission(thread: thread, linkPreview: payload) else { return }
                    submission = edit
                } else {
                    submission = .send(frozen)
                }
                await dispatch(submission, payload, onQueued)
            }
        )
    }

    func perform(
        _ submission: ComposerSubmission, session: SessionCoordinator, in conversation: ConversationID,
        linkPreview: LinkPreviewPayload?, onQueued: @MainActor @Sendable () -> Void
    ) async {
        switch submission {
        case let .send(draft):
            let id = await session.send(draft, in: conversation, linkPreview: linkPreview, onQueued: onQueued)
            if id == nil { errorMessage = "Couldn't send the message. Try again." }
        case let .sticker(sticker, pack, reply, thread):
            await session.send(sticker, from: pack, in: conversation, reply: reply, thread: thread)
        case let .edit(item, draft):
            if !(await session.edit(item, draft: draft, linkPreview: linkPreview)) {
                errorMessage = "Couldn't edit the message. Try again."
            }
        }
    }
}
