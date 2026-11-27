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
        return await showOwnPack(id, else: StickerPack(id: id, name: draft.name, summary: draft.summary, stickers: draft.stickers))
    }

    /// Takes one of the account's packs down. Stickers already sent from
    /// it stay in their conversations. Throws `PortError`.
    public func removeStickerPack(_ packID: String) async throws {
        try await whileOnline { try await port.retractStickerPack(packID) }
        stickers.remove(packID)
    }

    // MARK: - The pack behind a sticker

    /// Where the pack of a sticker row is: on the account that sent it.
    /// Nil when the row is not a sticker, names no pack, was sent by a
    /// room nick whose account is unknown, or says its pack is kept by
    /// someone other than its sender. Opening a pack sends a request from
    /// this account's address, so a message must not be able to choose
    /// who receives it.
    public func stickerPackLocation(of item: TimelineItem) -> StickerPackLocation? {
        MessageMedia.stickerPackLocation(of: item)
    }

    /// Fetches a pack from where it is kept; nil when it is no longer
    /// there. Throws `PortError`.
    public func stickerPack(at location: StickerPackLocation) async throws -> StickerPack? {
        try await whileOnline { try await port.fetchStickerPack(at: location) }
    }

    /// Adds the pack at `location` to the account's own packs (XEP-0449
    /// "Importing a sticker pack"): the same pack, under the same id. The
    /// pictures stay where their author put them. A pack its author
    /// restricted is refused. Throws `PortError`.
    @discardableResult
    public func copyStickerPack(_ pack: StickerPack, from location: StickerPackLocation) async throws -> StickerPack {
        guard !pack.isRestricted else { throw PortError.rejected }
        let id = try await whileOnline { try await port.copyStickerPack(from: location) }
        return await showOwnPack(id, else: StickerPack(id: id, name: pack.name, summary: pack.summary, stickers: pack.stickers))
    }

    /// Shows one of the account's packs as the server holds it, or as
    /// `sent` when it cannot be read back: it is published either way.
    private func showOwnPack(_ id: String, else sent: StickerPack) async -> StickerPack {
        let stored = try? await port.fetchStickerPack(at: StickerPackLocation(owner: account.jid, packID: id))
        let pack = stored ?? sent
        stickers.add(pack)
        return pack
    }
}
