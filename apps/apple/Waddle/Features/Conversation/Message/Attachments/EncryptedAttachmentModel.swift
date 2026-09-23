import Foundation
import Observation
import SwiftUI
import WaddleKit

/// A decrypted XEP-0448 file, held in memory for the row's lifetime.
struct DecryptedAttachment {
    let data: Data
    /// Set when the plaintext is a displayable image.
    let image: Image?
}

/// Fetches and decrypts one XEP-0448 file for its row.
@MainActor
@Observable
final class EncryptedAttachmentModel {
    enum Phase {
        case loading
        case decrypted(DecryptedAttachment)
        case failed
    }

    let file: SharedFile
    let source: EncryptedFileSource
    private(set) var phase: Phase = .loading
    /// The plaintext file Quick Look is showing.
    var previewURL: URL?

    init(file: SharedFile, source: EncryptedFileSource) {
        self.file = file
        self.source = source
    }

    func load() async {
        guard case .loading = phase else { return }
        do {
            let data = try await Self.fetchAndDecrypt(file: file, source: source)
            phase = .decrypted(DecryptedAttachment(data: data, image: file.isImage ? Image(data: data) : nil))
        } catch {
            // A cancelled load (the row scrolled away) stays `.loading` so
            // the next appearance starts over.
            if !Task.isCancelled {
                phase = .failed
            }
        }
    }

    func retry() async {
        phase = .loading
        await load()
    }

    func openPreview() {
        guard case let .decrypted(attachment) = phase, previewURL == nil else { return }
        previewURL = try? DecryptedFileStore.write(attachment.data, for: file)
    }

    /// Download on the network, decrypt off the main actor.
    private nonisolated static func fetchAndDecrypt(file: SharedFile, source: EncryptedFileSource) async throws -> Data {
        guard let url = source.downloadURL else { throw EncryptedFileDownloadError.noHTTPSSource }
        let blob = try await EncryptedFileDownloader.download(from: url, limit: EncryptedFileDecryptor.maximumCiphertextSize)
        return try await Task.detached(priority: .userInitiated) {
            try EncryptedFileDecryptor.decrypt(blob, source: source, file: file)
        }.value
    }
}
