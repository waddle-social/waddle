import Foundation
import WaddleKit

/// The account's XEP-0449 sticker packs, on its own `urn:xmpp:stickers:0`
/// PEP node.
extension FFIXmppPort {
    func fetchStickerPacks() async throws -> [StickerPack] {
        try await mappingPortErrors {
            try await client.fetchStickerPacks(ownerJid: nil)
        }.map(FFIInbound.stickerPack)
    }

    func fetchStickerPack(id: String) async throws -> StickerPack? {
        guard let owner = account else { throw PortError.invalidRequest }
        return try await mappingPortErrors {
            try await client.fetchStickerPack(ownerJid: owner.description, node: nil, packId: id)
        }.map(FFIInbound.stickerPack)
    }

    func publishStickerPack(_ draft: StickerPackDraft) async throws -> String {
        try await mappingPortErrors {
            try await client.publishStickerPack(pack: FFIOutbound.stickerPack(draft))
        }
    }

    /// The core answers only yes or no, so a refusal and a lost
    /// connection look the same here.
    func retractStickerPack(_ packID: String) async throws {
        guard await client.retractStickerPack(packId: packID) else { throw PortError.failed }
    }
}

extension FFIInbound {
    /// A pack with the stickers that have a picture to fetch.
    static func stickerPack(_ pack: WaddleStickerPack) -> StickerPack {
        StickerPack(
            id: pack.id,
            name: pack.name,
            summary: pack.summary,
            isRestricted: pack.restricted,
            stickers: pack.stickers.compactMap(sticker)
        )
    }

    /// Nil when none of the sticker's sources is a web address.
    static func sticker(_ sticker: WaddlePackSticker) -> Sticker? {
        let sources = sticker.sources.compactMap(webURL)
        guard !sources.isEmpty else { return nil }
        return Sticker(
            description: sticker.desc,
            mediaType: sticker.mediaType,
            size: sticker.size.flatMap { Int(exactly: $0) },
            width: sticker.width.map(Int.init),
            height: sticker.height.map(Int.init),
            digests: FileDigests(xep0300: sticker.hashes.map { (algorithm: $0.algo, base64: $0.valueB64) }),
            sources: sources
        )
    }
}

extension FFIOutbound {
    /// The core works the pack's id out from its content, so none is sent.
    static func stickerPack(_ draft: StickerPackDraft) -> WaddleStickerPack {
        WaddleStickerPack(
            id: "",
            name: draft.name,
            summary: draft.summary,
            restricted: false,
            stickers: draft.stickers.map { sticker in
                WaddlePackSticker(
                    desc: sticker.description,
                    mediaType: sticker.mediaType,
                    size: sticker.size.flatMap { UInt64(exactly: $0) },
                    width: sticker.width.flatMap { UInt32(exactly: $0) },
                    height: sticker.height.flatMap { UInt32(exactly: $0) },
                    hashes: fileHashes(sticker.digests),
                    sources: sticker.sources.map(\.absoluteString),
                    suggests: []
                )
            }
        )
    }
}
