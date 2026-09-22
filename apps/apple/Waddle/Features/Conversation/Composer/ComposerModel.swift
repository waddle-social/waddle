import Foundation
import Observation
import WaddleKit

/// One composer's draft: text, reply or edit target, picked mentions and
/// attachments. Lives in `ComposerDraftStore`, so it survives switching
/// conversations.
@MainActor
@Observable
final class ComposerModel {
    var text = ""
    private(set) var reply: ReplyContext?
    private(set) var editing: TimelineItem?
    private(set) var mentions: [RecordedMention] = []
    private(set) var attachments: [ComposerAttachment] = []
    var errorMessage: String?

    /// The draft that was in progress when an edit started.
    @ObservationIgnored private var stashedText: String?
    @ObservationIgnored private var stashedMentions: [RecordedMention] = []
    /// The editable text the edit started from, to skip no-op corrections.
    @ObservationIgnored private var editBaseline = ""
    /// Bytes kept for retrying failed uploads.
    @ObservationIgnored private var payloads: [UUID: AttachmentPayload] = [:]
    @ObservationIgnored private var uploads: [UUID: Task<Void, Never>] = [:]

    var hasText: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var isEditing: Bool { editing != nil }

    /// Non-blank text or finished attachments, and nothing still
    /// uploading or failed.
    var canSend: Bool {
        if isEditing { return hasText }
        guard attachments.allSatisfy({ $0.sharedFile != nil }) else { return false }
        return hasText || !attachments.isEmpty
    }

    // MARK: - Reply and edit

    func beginReply(to item: TimelineItem) {
        guard let context = ReplyContext(replyingTo: item) else { return }
        if isEditing { cancelEdit() }
        reply = context
    }

    func cancelReply() {
        reply = nil
    }

    func beginEdit(_ item: TimelineItem) {
        if !isEditing {
            stashedText = text
            stashedMentions = mentions
        }
        editing = item
        reply = nil
        mentions = []
        editBaseline = EditableMarkdown.text(for: item)
        text = editBaseline
    }

    func cancelEdit() {
        editing = nil
        text = stashedText ?? ""
        mentions = stashedMentions
        stashedText = nil
        stashedMentions = []
    }

    // MARK: - Mentions

    func insertMention(_ candidate: MentionCandidate, replacing query: MentionQuery) {
        text = MentionTokens.completing(text, query: query, with: candidate.name)
        mentions.append(RecordedMention(token: candidate.token, target: candidate.target))
    }

    // MARK: - Attachments

    func addAttachment(_ payload: AttachmentPayload, thumbnail: Data?, uploader: AttachmentUploader) {
        guard payload.data.count <= AttachmentPolicy.maxBytes else {
            errorMessage = "\(payload.filename) is larger than 25 MB."
            return
        }
        let attachment = ComposerAttachment(
            id: UUID(),
            filename: payload.filename,
            mediaType: payload.mediaType,
            byteCount: payload.data.count,
            thumbnail: thumbnail,
            phase: .uploading(fraction: 0)
        )
        attachments.append(attachment)
        payloads[attachment.id] = payload
        startUpload(attachment.id, uploader: uploader)
    }

    func retryAttachment(_ id: UUID, uploader: AttachmentUploader) {
        guard payloads[id] != nil else { return }
        startUpload(id, uploader: uploader)
    }

    func removeAttachment(_ id: UUID) {
        uploads[id]?.cancel()
        uploads[id] = nil
        payloads[id] = nil
        attachments.removeAll { $0.id == id }
    }

    private func startUpload(_ id: UUID, uploader: AttachmentUploader) {
        guard let payload = payloads[id] else { return }
        uploads[id]?.cancel()
        setPhase(.uploading(fraction: 0), for: id)
        let report: @Sendable (Double) -> Void = { [weak self] fraction in
            Task { @MainActor [self] in self?.setProgress(fraction, for: id) }
        }
        uploads[id] = Task { [weak self] in
            do {
                let file = try await uploader.upload(payload, progress: report)
                guard !Task.isCancelled else { return }
                self?.finishUpload(id, phase: .uploaded(file))
            } catch {
                guard !Task.isCancelled else { return }
                self?.finishUpload(id, phase: .failed(AttachmentUploadError.message(for: error)))
            }
        }
    }

    private func setProgress(_ fraction: Double, for id: UUID) {
        guard let index = attachments.firstIndex(where: { $0.id == id }),
              case .uploading = attachments[index].phase
        else { return }
        attachments[index].phase = .uploading(fraction: fraction)
    }

    private func finishUpload(_ id: UUID, phase: ComposerAttachment.Phase) {
        uploads[id] = nil
        if case .uploaded = phase {
            payloads[id] = nil
        }
        setPhase(phase, for: id)
    }

    private func setPhase(_ phase: ComposerAttachment.Phase, for id: UUID) {
        guard let index = attachments.firstIndex(where: { $0.id == id }) else { return }
        attachments[index].phase = phase
    }

    // MARK: - Sending

    /// Takes what the composer holds as a send or an edit and clears it at
    /// once, so a second Return before the round trip cannot resend.
    func takeSubmission(thread: String?) -> ComposerSubmission? {
        if let editing {
            let newText = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !newText.isEmpty else { return nil }
            let baseline = editBaseline.trimmingCharacters(in: .whitespacesAndNewlines)
            cancelEdit()
            guard newText != baseline else { return nil }
            return .edit(editing, newText)
        }
        guard canSend else { return nil }
        let draft = Draft(
            text: text,
            mentions: MentionTokens.locate(mentions, in: text),
            reply: reply,
            thread: thread,
            attachments: attachments.compactMap(\.sharedFile)
        )
        clearDraft()
        return .send(draft)
    }

    func perform(_ submission: ComposerSubmission, session: SessionCoordinator, in conversation: ConversationID) async {
        switch submission {
        case let .send(draft):
            await session.send(draft, in: conversation)
        case let .edit(item, newText):
            let succeeded = await session.edit(item, to: newText)
            if !succeeded {
                errorMessage = "Couldn't edit the message. Try again."
            }
        }
    }

    private func clearDraft() {
        text = ""
        reply = nil
        mentions = []
        attachments = []
        payloads = [:]
        uploads = [:]
    }
}

/// What one press of Send does.
enum ComposerSubmission {
    case send(Draft)
    /// XEP-0308 correction of `item` to the new text.
    case edit(TimelineItem, String)
}

/// Composer drafts per account, conversation and thread, kept in memory
/// for the app's lifetime.
@MainActor
final class ComposerDraftStore {
    static let shared = ComposerDraftStore()

    struct Key: Hashable {
        let account: BareJID
        let conversation: ConversationID
        let thread: String?
    }

    private var models: [Key: ComposerModel] = [:]

    func model(for key: Key) -> ComposerModel {
        if let existing = models[key] {
            return existing
        }
        let created = ComposerModel()
        models[key] = created
        return created
    }
}
