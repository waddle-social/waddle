import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import WaddleKit

/// The message composer: mention suggestions, reply or edit banner,
/// pending attachments, and the text field with attach and send buttons.
struct ConversationComposer: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    @Bindable var model: ComposerModel
    let conversation: ConversationID
    /// XEP-0201 thread replies go to, or nil for the main feed.
    let thread: String?
    let placeholder: String

    @FocusState private var isFocused: Bool
    @State private var showsPhotoPicker = false
    @State private var showsFileImporter = false
    @State private var photoSelection: [PhotosPickerItem] = []

    var body: some View {
        let suggestions = mentionSuggestions
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            if let suggestions, !suggestions.candidates.isEmpty {
                ComposerMentionSuggestions(candidates: suggestions.candidates) { candidate in
                    model.insertMention(candidate, replacing: suggestions.query)
                }
            }
            if let error = model.errorMessage {
                ComposerErrorLine(message: error) { model.errorMessage = nil }
            }
            ComposerContextBanner(model: model)
            if !model.attachments.isEmpty, !model.isEditing {
                ComposerAttachmentStrip(model: model, uploader: uploader)
            }
            inputRow(suggestions: suggestions)
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.top, Theme.Spacing.s)
        .padding(.bottom, Theme.Spacing.s)
        .background(.bar)
        .photosPicker(
            isPresented: $showsPhotoPicker,
            selection: $photoSelection,
            maxSelectionCount: 6,
            // Images only: picked assets load into memory, and video
            // location metadata is not stripped. Videos go through Files.
            matching: .images
        )
        .fileImporter(isPresented: $showsFileImporter, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            if case let .success(urls) = result {
                attachFiles(urls)
            }
        }
        .dropDestination(for: URL.self) { urls, _ in
            let files = urls.filter(\.isFileURL)
            attachFiles(files)
            return !files.isEmpty
        }
        .onChange(of: photoSelection) { _, items in
            guard !items.isEmpty else { return }
            photoSelection = []
            attachPhotos(items)
        }
        .onChange(of: model.text) { oldValue, newValue in
            reportTyping(old: oldValue, new: newValue)
        }
        .onChange(of: actions.focusRequest) { _, _ in
            isFocused = true
        }
    }

    private func inputRow(suggestions: (query: MentionQuery, candidates: [MentionCandidate])?) -> some View {
        HStack(alignment: .bottom, spacing: Theme.Spacing.s) {
            ComposerAttachmentMenu(
                isDisabled: model.isEditing,
                onPhoto: { showsPhotoPicker = true },
                onFile: { showsFileImporter = true }
            )
            ComposerTextField(
                text: $model.text,
                placeholder: model.isEditing ? "Edit message" : placeholder,
                isFocused: $isFocused,
                onSubmit: submit,
                onAcceptSuggestion: {
                    guard let suggestions, let first = suggestions.candidates.first else { return false }
                    model.insertMention(first, replacing: suggestions.query)
                    return true
                },
                onCancel: cancelContext
            )
            ComposerSendButton(isEditing: model.isEditing, isEnabled: model.canSend, action: submit)
        }
        .padding(.horizontal, Theme.Spacing.xs)
        .padding(.vertical, Theme.Spacing.xs + 2)
        #if os(iOS)
        .background(
            RoundedRectangle(cornerRadius: inputCornerRadius, style: .continuous)
                .fill(.regularMaterial)
        )
        .overlay(
            RoundedRectangle(cornerRadius: inputCornerRadius, style: .continuous)
                .strokeBorder(
                    isFocused ? Color.accentColor.opacity(0.45) : Color.primary.opacity(0.08),
                    lineWidth: 1
                )
        )
        .animation(.easeInOut(duration: 0.18), value: isFocused)
        #else
        .padding(.horizontal, Theme.Spacing.s - Theme.Spacing.xs)
        .padding(.vertical, Theme.Spacing.s - 2 - Theme.Spacing.xs - 2)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous).fill(Color.secondaryBackground))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous)
                .strokeBorder(isFocused ? Color.accentColor.opacity(0.5) : Color.secondary.opacity(0.2), lineWidth: 1)
        )
        #endif
    }

    private var inputCornerRadius: CGFloat {
        #if os(iOS)
        28
        #else
        Theme.Radius.large
        #endif
    }

    private var uploader: AttachmentUploader {
        AttachmentUploader(session: session)
    }

    /// Mention completion for the `@word` at the end of the draft, in rooms
    /// only (1:1 has nobody else to address).
    private var mentionSuggestions: (query: MentionQuery, candidates: [MentionCandidate])? {
        guard conversation.isRoom, !model.isEditing, let query = MentionTokens.trailingQuery(in: model.text) else {
            return nil
        }
        let occupants = Array((session.presence.occupants[conversation.jid] ?? [:]).values)
        let candidates = MentionCandidates.matching(query.text, occupants: occupants, ownNick: session.account.nick)
        return (query, candidates)
    }

    private func submit() {
        guard let submission = model.takeSubmission(thread: thread) else { return }
        let model = self.model
        let session = self.session
        let conversation = self.conversation
        Task { await model.perform(submission, session: session, in: conversation) }
    }

    /// Escape leaves edit mode, else drops the reply.
    private func cancelContext() -> Bool {
        if model.isEditing {
            model.cancelEdit()
            return true
        }
        if model.reply != nil {
            model.cancelReply()
            return true
        }
        return false
    }

    /// XEP-0085: composing on edits, active when the draft is cleared.
    /// Editing an old message is not composing a new one.
    private func reportTyping(old: String, new: String) {
        guard !model.isEditing, old != new else { return }
        if new.isEmpty {
            session.stopTyping(in: conversation, notify: true)
        } else {
            session.userTyped(in: conversation)
        }
    }

    private func attachFiles(_ urls: [URL]) {
        let uploader = self.uploader
        for url in urls {
            Task {
                do {
                    let payload = try await AttachmentLoader.payload(fromFile: url)
                    let thumbnail = payload.mediaType.hasPrefix("image/") ? AttachmentImageInfo.thumbnail(from: payload.data) : nil
                    model.addAttachment(payload, thumbnail: thumbnail, uploader: uploader)
                } catch {
                    model.errorMessage = "\(url.lastPathComponent): \(AttachmentUploadError.message(for: error))"
                }
            }
        }
    }

    private func attachPhotos(_ items: [PhotosPickerItem]) {
        let uploader = self.uploader
        for item in items {
            Task {
                let contentType = item.supportedContentTypes.first
                guard let data = try? await item.loadTransferable(type: Data.self) else {
                    model.errorMessage = "Couldn't read that photo."
                    return
                }
                let payload: AttachmentPayload
                do {
                    payload = try await AttachmentLoader.payload(
                        fromPhoto: data,
                        mediaType: contentType?.preferredMIMEType,
                        fileExtension: contentType?.preferredFilenameExtension
                    )
                } catch {
                    model.errorMessage = AttachmentUploadError.message(for: error)
                    return
                }
                let thumbnail = payload.mediaType.hasPrefix("image/") ? AttachmentImageInfo.thumbnail(from: payload.data) : nil
                model.addAttachment(payload, thumbnail: thumbnail, uploader: uploader)
            }
        }
    }
}
