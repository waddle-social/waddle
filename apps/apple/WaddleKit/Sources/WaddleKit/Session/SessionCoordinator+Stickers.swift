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
        let stored = try? await port.fetchStickerPack(at: StickerPackLocation(owner: account.jid, packID: id))
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

    // MARK: - The pack behind a sticker

    /// Where the pack of a sticker row is: where its marker says, else
    /// with the account that sent it. Nil when the row is not a sticker,
    /// names no pack, or was sent by a room nick whose account is unknown.
    public func stickerPackLocation(of item: TimelineItem) -> StickerPackLocation? {
        guard item.message.isSticker, let reference = item.message.stickerPack else { return nil }
        guard let owner = reference.owner ?? item.authorJID else { return nil }
        return StickerPackLocation(owner: owner, node: reference.node, packID: reference.packID)
    }

    /// Fetches a pack from where it is kept; nil when it is no longer
    /// there. Throws `PortError`.
    public func stickerPack(at location: StickerPackLocation) async throws -> StickerPack? {
        try await whileOnline { try await port.fetchStickerPack(at: location) }
    }

    /// Adds someone else's pack to the account's own, as the same
    /// stickers under the same name (XEP-0449 "Importing a sticker
    /// pack"). The pictures stay where their author put them. A pack its
    /// author restricted is refused. Throws `PortError`.
    @discardableResult
    public func copyStickerPack(_ pack: StickerPack) async throws -> StickerPack {
        guard !pack.isRestricted else { throw PortError.rejected }
        return try await createStickerPack(StickerPackDraft(name: pack.title, summary: pack.summary, stickers: pack.stickers))
    }
}
