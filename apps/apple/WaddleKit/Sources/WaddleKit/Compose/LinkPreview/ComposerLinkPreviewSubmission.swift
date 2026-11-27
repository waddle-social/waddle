import Foundation
import Observation

/// Owns the preview for the new-message draft and the separate edit draft.
@MainActor
@Observable
public final class ComposerLinkPreviewSubmission {
    public private(set) var isPreparing = false
    public private(set) var editRevision = 0
    private let draftPreview: ComposerLinkPreview
    private let editPreview: ComposerLinkPreview
    @ObservationIgnored private weak var session: SessionCoordinator?

    public init(
        now: @escaping @MainActor () -> Date = Date.init,
        sleep: @escaping ComposerLinkPreview.Sleep = { try await Task.sleep(for: $0) }
    ) {
        draftPreview = ComposerLinkPreview(now: now, sleep: sleep)
        editPreview = ComposerLinkPreview(now: now, sleep: sleep)
    }

    public func preview(isEditing: Bool) -> ComposerLinkPreview {
        isEditing ? editPreview : draftPreview
    }

    public func resetEdit() {
        editRevision += 1
        editPreview.reset()
    }

    public func update(text: String, isEditing: Bool, in conversation: ConversationID, session: SessionCoordinator) {
        if self.session !== session {
            draftPreview.reset()
            resetEdit()
            self.session = session
        }
        let body = Self.previewBody(for: Draft(text: text))
        preview(isEditing: isEditing).update(body: body, scopeJid: conversation.jid) { [weak session] request in
            guard let session else { throw PortError.notConnected }
            return try await session.lookupLinkPreview(request)
        }
    }

    /// Keep the frozen draft until the outbox accepts it. The callback
    /// releases the composer before the transport can suspend the send.
    public func submit(
        _ draft: Draft, editing item: TimelineItem?, in conversation: ConversationID, session: SessionCoordinator,
        currentDraft: @escaping @MainActor () -> Draft?,
        consumeNewDraft: @escaping @MainActor (_ consumesText: Bool) -> Void,
        perform: @MainActor (Draft, LinkPreviewPayload?, _ onQueued: @escaping @MainActor @Sendable () -> Void) async -> Void
    ) async {
        guard !isPreparing else { return }
        isPreparing = true
        var released = false
        func release() {
            guard !released else { return }
            released = true
            isPreparing = false
        }
        defer { release() }

        let isEditing = item != nil
        update(text: draft.text, isEditing: isEditing, in: conversation, session: session)
        let preview = preview(isEditing: isEditing)
        let revision = editRevision
        let edit = item.map { ComposerLinkPreviewEdit(item: $0, draft: draft, state: preview.state) }
        let payload: LinkPreviewPayload?
        if edit?.shouldResolve != false {
            payload = await preview.payloadForSend(body: Self.previewBody(for: draft), scopeJid: conversation.jid)
        } else {
            payload = nil
        }
        guard !Task.isCancelled, self.session === session else { return }
        if isEditing {
            guard revision == editRevision, currentDraft() == draft, !session.isStopped else { return }
            // The app takes the edit synchronously before dispatch, using the
            // same no-op check as callers that take it without a lookup.
            release()
        }
        await perform(draft, payload) { [self] in
            guard !isEditing, !released else { return }
            let current = currentDraft()
            let consumesText = current?.text == draft.text && current?.references == draft.references
            draftPreview.reset()
            consumeNewDraft(consumesText)
            if !consumesText, let current {
                update(text: current.text, isEditing: false, in: conversation, session: session)
            }
            release()
        }
    }

    private static func previewBody(for draft: Draft) -> String {
        // Inspect serialized text before adding quoted replies or file URLs.
        MessageComposer.compose(Draft(text: draft.text, references: draft.references))?.body ?? ""
    }
}
