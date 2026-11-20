import WaddleKit
import XCTest
@testable import Waddle

/// What an outgoing message tells the Rust core about its files.
final class OutboundBridgeTests: XCTestCase {
    private let slot = URL(string: "https://upload.waddle.example/7f3c/summit.jpg.enc")!

    /// XEP-0448 §2.1: an encrypted file's metadata carries a digest of the
    /// file itself, and the envelope names the cipher, key, IV, the
    /// ciphertext's digest and where the ciphertext is. The core refuses
    /// an encrypted file without the first.
    func testAnEncryptedFileCarriesItsDigestAndEnvelope() throws {
        let file = SharedFile(
            url: slot,
            name: "summit.jpg",
            mediaType: "image/jpeg",
            size: 41,
            disposition: .inline,
            digests: FileDigests([.sha256: Data(repeating: 0xAB, count: 32)]),
            encrypted: EncryptedFileSource(
                cipher: .aes256GCM,
                keyBase64: "SuRJ2agVm/pQbJQlPq/B23Xt1YOOJCcEGJA5HrcYOGQ=",
                ivBase64: "T8RDMBaiqn6Ci4Nw",
                digests: FileDigests([.sha256: Data(repeating: 0xCD, count: 32)]),
                sources: [slot]
            )
        )

        let sent = FFIOutbound.sharedFile(file)

        XCTAssertEqual(sent.hashes, [
            WaddleStickerHash(algo: "sha-256", valueB64: "q6urq6urq6urq6urq6urq6urq6urq6urq6urq6urq6s="),
        ])
        XCTAssertEqual(sent.encrypted, WaddleEncryptedFile(
            cipher: "urn:xmpp:ciphers:aes-256-gcm-nopadding:0",
            keyB64: "SuRJ2agVm/pQbJQlPq/B23Xt1YOOJCcEGJA5HrcYOGQ=",
            ivB64: "T8RDMBaiqn6Ci4Nw",
            hashes: [WaddleEncryptedFileHash(algo: "sha-256", valueB64: "zc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc0=")],
            sources: ["https://upload.waddle.example/7f3c/summit.jpg.enc"]
        ))
        XCTAssertEqual(sent.name, "summit.jpg")
        XCTAssertEqual(sent.mediaType, "image/jpeg")
        XCTAssertEqual(sent.size, 41)
    }

    // MARK: - Stickers

    private let wave = Sticker(
        description: "👋",
        size: 2048,
        width: 512,
        height: 384,
        digests: FileDigests([.sha256: Data(repeating: 0xAB, count: 32)]),
        sources: [URL(string: "https://upload.waddle.example/stickers/wave.png")!]
    )

    /// XEP-0449: `<sticker pack='…'/>` beside the picture, which is an
    /// inline file with its description and digest, whatever its type.
    func testAStickerNamesItsPackAndCarriesItsPictureInline() throws {
        let composed = try XCTUnwrap(MessageComposer.compose(wave, from: StickerPack(id: "EpRv28DHHzFrE4zd+xaNpVb4", stickers: [wave])))

        let sent = FFIOutbound.sendOptions(composed.options, stanzaID: "c1")

        XCTAssertEqual(sent.sticker, WaddleStickerRef(packId: "EpRv28DHHzFrE4zd+xaNpVb4", packJid: nil, packNode: nil))
        XCTAssertEqual(sent.sharedFiles.count, 1)
        let picture = try XCTUnwrap(sent.sharedFiles.first)
        XCTAssertEqual(picture.url, "https://upload.waddle.example/stickers/wave.png")
        XCTAssertEqual(picture.desc, "👋")
        XCTAssertEqual(picture.disposition, "inline")
        XCTAssertEqual(picture.hashes, [
            WaddleStickerHash(algo: "sha-256", valueB64: "q6urq6urq6urq6urq6urq6urq6urq6urq6urq6urq6s="),
        ])
        XCTAssertNil(picture.encrypted)
    }

    func testAnOrdinaryMessageIsNotASticker() {
        XCTAssertNil(FFIOutbound.sendOptions(OutboundOptions(), stanzaID: "c1").sticker)
    }

    func testANewPackIsHandedToTheCoreWithItsStickers() {
        let draft = StickerPackDraft(name: "Penguins", summary: "Waddling", stickers: [wave])

        XCTAssertEqual(FFIOutbound.stickerPack(draft), WaddleStickerPack(
            id: "",
            name: "Penguins",
            summary: "Waddling",
            restricted: false,
            stickers: [
                WaddlePackSticker(
                    desc: "👋",
                    mediaType: nil,
                    size: 2048,
                    width: 512,
                    height: 384,
                    hashes: [WaddleStickerHash(algo: "sha-256", valueB64: "q6urq6urq6urq6urq6urq6urq6urq6urq6urq6urq6s=")],
                    sources: ["https://upload.waddle.example/stickers/wave.png"],
                    suggests: []
                ),
            ]
        ))
    }

    /// A pack read from the server: sources that are not web addresses are
    /// left out, and a sticker with none left cannot be shown or sent.
    func testAPackFromTheServerKeepsOnlyStickersWithAPicture() {
        let fetched = WaddleStickerPack(
            id: "EpRv28DHHzFrE4zd+xaNpVb4",
            name: "Penguins",
            summary: nil,
            restricted: true,
            stickers: [
                WaddlePackSticker(
                    desc: "👋", mediaType: "image/png", size: 2048, width: 512, height: 384,
                    hashes: [WaddleStickerHash(algo: "sha-256", valueB64: "q6urq6urq6urq6urq6urq6urq6urq6urq6urq6urq6s=")],
                    sources: ["javascript:alert(1)", "https://upload.waddle.example/stickers/wave.png"],
                    suggests: []
                ),
                WaddlePackSticker(
                    desc: "🐧", mediaType: "image/png", size: nil, width: nil, height: nil,
                    hashes: [], sources: ["file:///etc/passwd"], suggests: []
                ),
            ]
        )

        XCTAssertEqual(FFIInbound.stickerPack(fetched), StickerPack(
            id: "EpRv28DHHzFrE4zd+xaNpVb4",
            name: "Penguins",
            stickers: [
                Sticker(
                    description: "👋", mediaType: "image/png", size: 2048, width: 512, height: 384,
                    digests: FileDigests([.sha256: Data(repeating: 0xAB, count: 32)]),
                    sources: [URL(string: "https://upload.waddle.example/stickers/wave.png")!]
                ),
            ]
        ))
    }
}
