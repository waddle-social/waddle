import Foundation
import Observation
import WaddleKit

/// One composer's draft: text, reply or edit target, picked references and
/// attachments. Lives in `ComposerDraftStore`, so it survives switching
/// conversations.
@MainActor
@Observable
final class ComposerModel {
    private(set) var text = ""
    private(set) var reply: ReplyContext?
    private(set) var editing: TimelineItem?
    private(set) var references = RecordedDraftReferences()
    private(set) var attachments: [ComposerAttachment] = []
    var errorMessage: String?
    var linkPreviews = ComposerLinkPreviewSubmission()

    /// The draft that was in progress when an edit started.
    @ObservationIgnored private var stashedDraft: (text: String, references: RecordedDraftReferences)?
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
        guard !linkPreviews.isPreparing else { return false }
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
        guard !isDraftLocked else { return }
        if !isEditing {
            stashedDraft = (text, references)
        }
        let editable = EditableMessage(item: item)
        editing = item
        reply = nil
        linkPreviews.resetEdit()
        (text, references) = (editable.text, editable.recordedReferences)
    }

    func cancelEdit() {
        linkPreviews.resetEdit()
        editing = nil
        (text, references) = stashedDraft ?? ("", .init())
        stashedDraft = nil
    }

    // MARK: - Draft text

    /// Empties the text and its picked references, keeping the
    /// reply target and attachments (a slash command consumed the text).
    func clearText() {
        (text, references) = ("", .init())
    }

    func replaceText(_ replacement: ComposerTextReplacement) {
        references.relocate(from: text, applying: replacement)
        text = replacement.text
    }

    func accept(_ pick: ComposerSuggestionsModel.Pick) {
        replaceText(ComposerTextReplacement(pick.edit.text, replacing: pick.replacementRange))
        if let reference = pick.reference { references.append(reference) }
        if pick.clearsError { errorMessage = nil }
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
    func takeSubmission(thread: String?, linkPreview payload: LinkPreviewPayload? = nil) -> ComposerSubmission? {
        guard canSend else { return nil }
        let draft = submissionDraft(thread: thread)
        if let editing {
            let edit = ComposerLinkPreviewEdit(item: editing, draft: draft, state: linkPreview.state)
            cancelEdit()
            return edit.shouldSend(payload) ? .edit(editing, draft) : nil
        }
        clearDraft()
        return .send(draft)
    }

    /// A message whose body is just `url` (a picked GIF), in reply to the
    /// current reply target and in `thread`. The draft text and pending
    /// attachments stay in the composer.
    func takeLinkSubmission(_ url: URL, thread: String?) -> ComposerSubmission {
        let draft = Draft(text: url.absoluteString, reply: reply, thread: thread)
        reply = nil
        return .send(draft)
    }

    /// A sticker, in reply to the current reply target and in `thread`.
    /// The draft text and pending attachments stay in the composer.
    func takeStickerSubmission(_ sticker: Sticker, from pack: StickerPack, thread: String?) -> ComposerSubmission {
        defer { reply = nil }
        return .sticker(sticker, pack, reply: reply, thread: thread)
    }

    private func clearDraft() {
        clearText()
        reply = nil
        attachments = []
        payloads = [:]
        uploads = [:]
    }
}

/// What one press of Send does.
enum ComposerSubmission {
    case send(Draft)
    case sticker(Sticker, StickerPack, reply: ReplyContext?, thread: String?)
    /// XEP-0308 correction of `item` to the draft's text and references.
    case edit(TimelineItem, Draft)
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
