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
}
