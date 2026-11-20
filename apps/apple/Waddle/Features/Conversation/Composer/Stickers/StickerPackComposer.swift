import Observation
import SwiftUI
import WaddleKit

/// A sticker pack being put together: a name, and pictures with a few
/// words each.
@MainActor
@Observable
final class StickerPackComposer {
    struct Entry: Identifiable {
        let id = UUID()
        let payload: AttachmentPayload
        let preview: Image?
        /// What the sticker shows; people whose app does not draw
        /// stickers read this instead.
        var description = ""
    }

    enum Phase: Equatable {
        case editing
        /// Uploading pictures: how many are up, of how many.
        case creating(done: Int, total: Int)
    }

    static let maxStickers = 30

    var name: String
    var summary: String
    var entries: [Entry]
    private(set) var phase: Phase
    var errorMessage: String?
    /// Pictures chosen and still being read.
    private(set) var pendingPictures = 0

    init(name: String = "", summary: String = "", entries: [Entry] = [], phase: Phase = .editing, errorMessage: String? = nil) {
        self.name = name
        self.summary = summary
        self.entries = entries
        self.phase = phase
        self.errorMessage = errorMessage
    }

    var isCreating: Bool { phase != .editing }
    var isFull: Bool { entries.count + pendingPictures >= Self.maxStickers }

    var canCreate: Bool {
        !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !entries.isEmpty
            && pendingPictures == 0
            && !isCreating
    }

    /// Adds a chosen picture. `read` fetches its bytes, which can take a
    /// while for a picture kept in iCloud.
    func add(mediaType: String?, reading read: () async -> Data?) async {
        guard !isFull else {
            errorMessage = "A pack can have up to \(Self.maxStickers) stickers."
            return
        }
        pendingPictures += 1
        defer { pendingPictures -= 1 }
        errorMessage = nil
        guard let data = await read(),
              let payload = await Task.detached(priority: .userInitiated, operation: {
                  StickerPicture.payload(from: data, mediaType: mediaType)
              }).value
        else {
            errorMessage = "Couldn't read that picture. Try another one."
            return
        }
        entries.append(Entry(payload: payload, preview: Image(data: payload.data)))
    }

    func remove(_ id: Entry.ID) {
        entries.removeAll { $0.id == id }
    }

    /// Uploads every picture, then publishes the pack. Nil when it did
    /// not go through; `errorMessage` then says why. Nothing is published
    /// unless every picture is up, so a pack never misses a sticker.
    func create(using session: SessionCoordinator) async -> StickerPack? {
        guard canCreate else { return nil }
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let entries = entries
        phase = .creating(done: 0, total: entries.count)
        errorMessage = nil
        defer { phase = .editing }
        // A pack's pictures are for anyone who is sent one of its
        // stickers, so they are stored as they are.
        let uploader = AttachmentUploader(session: session, protection: .plain)
        do {
            var stickers: [Sticker] = []
            for entry in entries {
                let file = try await uploader.upload(entry.payload) { _ in
                    // Progress is counted in pictures, not bytes.
                }
                try Task.checkCancellation()
                stickers.append(Self.sticker(for: entry, at: file.url, packName: name))
                phase = .creating(done: stickers.count, total: entries.count)
            }
            return try await session.createStickerPack(StickerPackDraft(name: name, summary: summary, stickers: stickers))
        } catch where Task.isCancelled {
            return nil
        } catch let error as AttachmentUploadError {
            errorMessage = error.message
        } catch {
            errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't make the pack. Try again.")
        }
        return nil
    }

    /// A sticker left without words is described by its pack's name, so
    /// the text sent with it is never empty.
    static func sticker(for entry: Entry, at url: URL, packName: String) -> Sticker {
        let description = entry.description.trimmingCharacters(in: .whitespacesAndNewlines)
        return Sticker(
            description: description.isEmpty ? packName : description,
            mediaType: entry.payload.mediaType,
            size: entry.payload.data.count,
            width: entry.payload.width,
            height: entry.payload.height,
            digests: .sha256(of: entry.payload.data),
            sources: [url]
        )
    }
}
