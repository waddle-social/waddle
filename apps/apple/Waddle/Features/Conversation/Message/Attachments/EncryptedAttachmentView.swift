import QuickLook
import SwiftUI
import WaddleKit

/// A XEP-0448 file, shown like a plain attachment from its plaintext
/// `<file/>` metadata once decrypted.
struct EncryptedAttachmentView: View {
    @State private var model: EncryptedAttachmentModel
    let kind: MessageAttachmentKind
    let isSticker: Bool

    init(file: SharedFile, source: EncryptedFileSource, kind: MessageAttachmentKind, isSticker: Bool) {
        _model = State(initialValue: EncryptedAttachmentModel(file: file, source: source))
        self.kind = kind
        self.isSticker = isSticker
    }

    var body: some View {
        content
            .task { await model.loadImage() }
            .quickLookPreview($model.previewURL)
            .onChange(of: model.previewURL) { closedURL, currentURL in
                if currentURL == nil, let closedURL {
                    DecryptedFileStore.remove(closedURL)
                }
            }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .idle:
            EncryptedAttachmentIdleCard(file: model.file, kind: kind) {
                Task { await model.openFile() }
            }
        case .loading:
            EncryptedAttachmentLoadingCard(file: model.file, kind: kind)
        case .failed:
            EncryptedAttachmentFailedCard(file: model.file) {
                Task { await model.retry() }
            }
        case let .decrypted(attachment):
            if let image = attachment.image {
                DecryptedImageAttachment(file: model.file, image: image, isSticker: isSticker) {
                    model.openPreview()
                }
            } else {
                DecryptedFileCard(file: model.file, kind: kind) {
                    model.openPreview()
                }
            }
        }
    }
}
