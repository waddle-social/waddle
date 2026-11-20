import Crypto
import Foundation
import Testing
import WaddleKit

@Suite("XEP-0448 encrypted uploads")
struct EncryptedUploadTests {
    private let plaintext = Data("Photo from the summit, rendered as bytes.".utf8)
    private let slot = URL(string: "https://upload.waddle.example/7f3c/photo.jpg.enc")!

    @Test func anEncryptedFileOpensWithTheEnvelopeItWasSealedWith() throws {
        let upload = try EncryptedUpload(sealing: plaintext)
        let source = upload.source(at: slot)

        let opened = try EncryptedFileDecryptor.decrypt(
            upload.ciphertext,
            key: EncryptedFileKey(source),
            encryptedDigests: source.digests,
            plaintextDigests: upload.plaintextDigests,
            declaredSize: plaintext.count
        )

        #expect(opened == plaintext)
        #expect(source.cipher == .aes256GCM)
        #expect(source.sources == [slot])
    }

    /// XEP-0448 §5 recommends appending the GCM tag: the upload is exactly
    /// 16 bytes longer than the file, and none of the file shows through.
    @Test func theUploadIsTheCiphertextWithItsTag() throws {
        let upload = try EncryptedUpload(sealing: plaintext)

        #expect(upload.ciphertext.count == plaintext.count + 16)
        #expect(upload.ciphertext.range(of: Data("summit".utf8)) == nil)
    }

    /// §2.1 requires a digest of the file; the envelope carries one of the
    /// ciphertext. Both are SHA-256, as the other Waddle clients send.
    @Test func digestsCoverTheFileAndTheCiphertext() throws {
        let upload = try EncryptedUpload(sealing: plaintext)

        #expect(upload.plaintextDigests.values == [.sha256: Data(SHA256.hash(data: plaintext))])
        #expect(upload.source(at: slot).digests.values == [.sha256: Data(SHA256.hash(data: upload.ciphertext))])
    }

    @Test func everyFileGetsItsOwnKeyAndIV() throws {
        let first = try EncryptedUpload(sealing: plaintext).source(at: slot)
        let second = try EncryptedUpload(sealing: plaintext).source(at: slot)

        #expect(first.keyBase64 != second.keyBase64)
        #expect(first.ivBase64 != second.ivBase64)
    }
}
