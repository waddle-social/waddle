import Foundation
import WaddleKit

/// The scripted sticker packs of a `FakePort`.
@MainActor
final class FakeStickers {
    var packs: Result<[StickerPack], PortError> = .success([])
    var publishResult: Result<Void, PortError> = .success(())
    var retractResult: Result<Void, PortError> = .success(())
    /// The id the next publish answers with.
    var publishedID = "pack-new"

    var packRequests = 0
    var published: [StickerPackDraft] = []
    var retracted: [String] = []
}

extension FakePort {
    func fetchStickerPacks() async throws -> [StickerPack] {
        stickers.packRequests += 1
        return try stickers.packs.get()
    }

    /// A published pack reads back as it was sent.
    func fetchStickerPack(id: String) async throws -> StickerPack? {
        guard id == stickers.publishedID, let draft = stickers.published.last else {
            return try stickers.packs.get().first { $0.id == id }
        }
        return StickerPack(id: id, name: draft.name, summary: draft.summary, stickers: draft.stickers)
    }

    func publishStickerPack(_ draft: StickerPackDraft) async throws -> String {
        try stickers.publishResult.get()
        stickers.published.append(draft)
        return stickers.publishedID
    }

    func retractStickerPack(_ packID: String) async throws {
        try stickers.retractResult.get()
        stickers.retracted.append(packID)
    }
}
