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

/// Fetches and decrypts one XEP-0448 file for its row. Images decrypt as
/// the row appears, within a small download cap, so the timeline shows
/// them; any other file downloads only when tapped and goes straight to
/// Quick Look, so a row never pulls a large file the user did not ask for.
@MainActor
@Observable
final class EncryptedAttachmentModel {
    enum Phase {
        /// A non-image file the user has not opened yet.
        case idle
        case loading
        case decrypted(DecryptedAttachment)
        case failed
    }

    /// The largest ciphertext a row fetches without a tap.
    static let automaticDownloadLimit = 20 * 1024 * 1024

    let file: SharedFile
    let source: EncryptedFileSource
    private(set) var phase: Phase
    /// The plaintext file Quick Look is showing.
    var previewURL: URL?

    init(file: SharedFile, source: EncryptedFileSource) {
        self.file = file
        self.source = source
        phase = file.isImage ? .loading : .idle
    }

    func loadImage() async {
        guard file.isImage, case .loading = phase else { return }
        do {
            let data = try await Self.fetchAndDecrypt(file: file, source: source, limit: Self.automaticDownloadLimit)
            phase = .decrypted(DecryptedAttachment(data: data, image: Image(data: data)))
        } catch {
            // A cancelled load (the row scrolled away) stays `.loading` so
            // the next appearance starts over.
            if !Task.isCancelled {
                phase = .failed
            }
        }
    }

    /// Downloads, decrypts and previews a file that is not shown inline.
    /// Its plaintext lives only in the preview file.
    func openFile() async {
        switch phase {
        case .idle, .failed:
            break
        case .loading, .decrypted:
            return
        }
        phase = .loading
        do {
            let data = try await Self.fetchAndDecrypt(file: file, source: source, limit: EncryptedFileDecryptor.maximumCiphertextSize)
            previewURL = try DecryptedFileStore.write(data, for: file)
            phase = .idle
        } catch {
            phase = Task.isCancelled ? .idle : .failed
        }
    }

    func retry() async {
        guard case .failed = phase else { return }
        if file.isImage {
            phase = .loading
            await loadImage()
        } else {
            await openFile()
        }
    }

    func openPreview() {
        guard case let .decrypted(attachment) = phase, previewURL == nil else { return }
        previewURL = try? DecryptedFileStore.write(attachment.data, for: file)
    }

    /// Download on the network, decrypt off the main actor.
    private nonisolated static func fetchAndDecrypt(file: SharedFile, source: EncryptedFileSource, limit: Int) async throws -> Data {
        guard let url = source.downloadURL else { throw EncryptedFileDownloadError.noHTTPSSource }
        let blob = try await EncryptedFileDownloader.download(from: url, limit: limit)
        return try await Task.detached(priority: .userInitiated) {
            try EncryptedFileDecryptor.decrypt(blob, source: source, file: file)
        }.value
    }
}
