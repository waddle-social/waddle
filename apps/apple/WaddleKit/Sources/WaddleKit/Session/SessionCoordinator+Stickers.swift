import Foundation

extension SessionCoordinator {
    /// Loads the account's sticker packs. A failed load keeps the packs
    /// already shown.
    public func loadStickerPacks() async {
        let load = stickers.begin()
        guard connection == .online else {
            stickers.finish(load, with: nil)
            return
        }
        let port = self.port
        stickers.finish(load, with: await Self.attempt { try await port.fetchStickerPacks() })
    }

    /// Publishes a pack whose pictures are already uploaded, and shows it
    /// as the server holds it. Throws `PortError`.
    @discardableResult
    public func createStickerPack(_ draft: StickerPackDraft) async throws -> StickerPack {
        guard let draft = draft.normalized else { throw PortError.invalidRequest }
        let id = try await whileOnline { try await port.publishStickerPack(draft) }
        // The pack is published either way; a failed read-back shows it
        // as it was sent.
        let stored = try? await port.fetchStickerPack(id: id)
        let pack = stored ?? StickerPack(id: id, name: draft.name, summary: draft.summary, stickers: draft.stickers)
        stickers.add(pack)
        return pack
    }

    /// Takes one of the account's packs down. Stickers already sent from
    /// it stay in their conversations. Throws `PortError`.
    public func removeStickerPack(_ packID: String) async throws {
        try await whileOnline { try await port.retractStickerPack(packID) }
        stickers.remove(packID)
    }
}
