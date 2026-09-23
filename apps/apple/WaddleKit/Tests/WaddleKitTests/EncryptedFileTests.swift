import Crypto
import CryptoExtras
import Foundation
import Testing
@testable import WaddleKit

@Suite("XEP-0448 encrypted files")
struct EncryptedFileTests {
    private let plaintext = Data("Photo from the summit, rendered as bytes.".utf8)

    // MARK: Key material

    @Test func ciphersParseFromTheirNamespaces() {
        #expect(FileCipher(rawValue: "urn:xmpp:ciphers:aes-128-gcm-nopadding:0") == .aes128GCM)
        #expect(FileCipher(rawValue: "urn:xmpp:ciphers:aes-256-gcm-nopadding:0") == .aes256GCM)
        #expect(FileCipher(rawValue: "urn:xmpp:ciphers:aes-256-cbc-pkcs7:0") == .aes256CBC)
        #expect(FileCipher(rawValue: "urn:xmpp:ciphers:chacha20-poly1305:0") == nil)
    }

    @Test func cipherLengthsFollowTheCipherTable() {
        #expect((FileCipher.aes128GCM.keyByteCount, FileCipher.aes128GCM.ivByteCount) == (16, 12))
        #expect((FileCipher.aes256GCM.keyByteCount, FileCipher.aes256GCM.ivByteCount) == (32, 12))
        #expect((FileCipher.aes256CBC.keyByteCount, FileCipher.aes256CBC.ivByteCount) == (32, 16))
    }

    @Test func specExampleKeyMaterialParses() throws {
        let source = EncryptedFileSource(
            cipher: .aes256GCM,
            keyBase64: "SuRJ2agVm/pQbJQlPq/B23Xt1YOOJCcEGJA5HrcYOGQ=",
            ivBase64: "T8RDMBaiqn6Ci4Nw",
            digests: FileDigests(),
            sources: [URL(string: "https://download.montague.lit/4a771ac1-f0b2-4a4a-9700-f2a26fa2bb67/encrypted.jpg")!]
        )
        let key = try EncryptedFileKey(source)
        #expect(key.key.count == 32)
        #expect(key.iv.count == 12)
    }

    @Test func wrongKeyLengthIsRejected() {
        #expect(throws: EncryptedFileError.wrongKeyLength(expected: 32, actual: 16)) {
            try EncryptedFileKey(cipher: .aes256GCM, key: Data(count: 16), iv: Data(count: 12))
        }
        #expect(throws: EncryptedFileError.wrongKeyLength(expected: 16, actual: 32)) {
            try EncryptedFileKey(cipher: .aes128GCM, key: Data(count: 32), iv: Data(count: 12))
        }
    }

    @Test func wrongIVLengthIsRejected() {
        #expect(throws: EncryptedFileError.wrongIVLength(expected: 16, actual: 12)) {
            try EncryptedFileKey(cipher: .aes256CBC, key: Data(count: 32), iv: Data(count: 12))
        }
    }

    @Test func nonBase64KeyMaterialIsRejected() {
        let source = EncryptedFileSource(
            cipher: .aes256GCM,
            keyBase64: "not base64!",
            ivBase64: "T8RDMBaiqn6Ci4Nw",
            digests: FileDigests(),
            sources: []
        )
        #expect(throws: EncryptedFileError.malformedKeyMaterial) { try EncryptedFileKey(source) }
    }

    // MARK: Digests

    @Test func digestsSkipUnsupportedAndMalformedValues() {
        let digests = FileDigests(xep0300: [
            (algorithm: "sha3-256", base64: "2XarmwTlNxDAMkvymloX3S5+VbylNrJt/l5QyPa+YoU="),
            (algorithm: "sha-256", base64: "not base64!"),
            (algorithm: "sha-512", base64: Data(repeating: 1, count: 64).base64EncodedString()),
            (algorithm: "sha-512", base64: Data(repeating: 2, count: 64).base64EncodedString()),
        ])
        #expect(digests.values == [.sha512: Data(repeating: 1, count: 64)])
    }

    @Test func digestCheckOutcomes() {
        let sha256 = Data(SHA256.hash(data: plaintext))
        let sha512 = Data(SHA512.hash(data: plaintext))
        #expect(DigestCheck(plaintext, against: FileDigests([.sha256: sha256, .sha512: sha512])) == .matched)
        #expect(DigestCheck(plaintext, against: FileDigests([.sha256: sha256, .sha512: Data(count: 64)])) == .mismatched)
        #expect(DigestCheck(plaintext, against: FileDigests()) == .noSupportedDigest)
    }

    @Test func downloadsFromTheFirstHTTPSSource() {
        let source = EncryptedFileSource(
            cipher: .aes256GCM,
            keyBase64: "",
            ivBase64: "",
            digests: FileDigests(),
            sources: [URL(string: "http://files.waddle.test/a")!, URL(string: "https://files.waddle.test/b")!]
        )
        #expect(source.downloadURL == URL(string: "https://files.waddle.test/b"))
    }

    // MARK: GCM

    @Test(arguments: [FileCipher.aes128GCM, .aes256GCM])
    func taggedGCMRoundTripsWithoutDigests(cipher: FileCipher) throws {
        let fixture = try Fixture(cipher: cipher)
        let blob = try fixture.sealTagged(plaintext)
        #expect(try fixture.decrypt(blob) == plaintext)
    }

    @Test func taglessGCMWithMatchingPlaintextDigestIsAccepted() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = try fixture.sealTagless(plaintext)
        let decrypted = try fixture.decrypt(blob, plaintextDigests: sha256(plaintext), declaredSize: plaintext.count)
        #expect(decrypted == plaintext)
    }

    @Test func taglessGCMWithMatchingCiphertextDigestIsAccepted() throws {
        let fixture = try Fixture(cipher: .aes128GCM)
        let blob = try fixture.sealTagless(plaintext)
        let decrypted = try fixture.decrypt(blob, encryptedDigests: sha256(blob), declaredSize: plaintext.count)
        #expect(decrypted == plaintext)
    }

    /// Without `<size/>` a failed tag leaves the layout a guess: the
    /// ciphertext digest cannot show the former tag was not decrypted as
    /// data, so only a plaintext digest is accepted.
    @Test func badTagWithoutSizeNeedsAPlaintextDigest() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        var blob = try fixture.sealTagged(plaintext)
        blob[blob.endIndex - 1] ^= 0x01
        #expect(throws: EncryptedFileError.unauthenticated) {
            try fixture.decrypt(blob, encryptedDigests: sha256(blob))
        }
        let tagless = try fixture.sealTagless(plaintext)
        #expect(throws: EncryptedFileError.unauthenticated) {
            try fixture.decrypt(tagless, encryptedDigests: sha256(tagless))
        }
        #expect(try fixture.decrypt(tagless, plaintextDigests: sha256(plaintext)) == plaintext)
    }

    @Test func taglessGCMShorterThanATagIsAcceptedWithDigest() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let short = Data("tiny".utf8)
        let blob = try fixture.sealTagless(short)
        #expect(try fixture.decrypt(blob, plaintextDigests: sha256(short)) == short)
    }

    @Test func taglessGCMWithoutDigestIsRejected() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = try fixture.sealTagless(plaintext)
        #expect(throws: EncryptedFileError.unauthenticated) { try fixture.decrypt(blob) }
    }

    @Test func tamperedTaggedCiphertextIsRejected() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        var blob = try fixture.sealTagged(plaintext)
        blob[blob.startIndex] ^= 0x01
        #expect(throws: EncryptedFileError.authenticationFailed) {
            try fixture.decrypt(blob, plaintextDigests: sha256(plaintext), declaredSize: plaintext.count)
        }
        #expect(throws: EncryptedFileError.plaintextDigestMismatch) {
            try fixture.decrypt(blob, plaintextDigests: sha256(plaintext))
        }
        #expect(throws: EncryptedFileError.unauthenticated) { try fixture.decrypt(blob) }
    }

    @Test func tamperedTagIsRejected() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        var blob = try fixture.sealTagged(plaintext)
        blob[blob.endIndex - 1] ^= 0x01
        #expect(throws: EncryptedFileError.authenticationFailed) {
            try fixture.decrypt(blob, plaintextDigests: sha256(plaintext), declaredSize: plaintext.count)
        }
    }

    @Test func wrongKeyIsRejected() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = try fixture.sealTagged(plaintext)
        let wrong = try Fixture(cipher: .aes256GCM)
        #expect(throws: EncryptedFileError.authenticationFailed) {
            try wrong.decrypt(blob, declaredSize: plaintext.count)
        }
        #expect(throws: EncryptedFileError.plaintextDigestMismatch) {
            try wrong.decrypt(blob, plaintextDigests: sha256(plaintext))
        }
    }

    @Test func ciphertextDigestMismatchIsRejectedBeforeDecrypting() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = try fixture.sealTagged(plaintext)
        #expect(throws: EncryptedFileError.ciphertextDigestMismatch) {
            try fixture.decrypt(blob, encryptedDigests: sha256(Data("other".utf8)))
        }
    }

    @Test func plaintextDigestMismatchIsRejected() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = try fixture.sealTagged(plaintext)
        #expect(throws: EncryptedFileError.plaintextDigestMismatch) {
            try fixture.decrypt(blob, plaintextDigests: sha256(Data("other".utf8)))
        }
    }

    @Test func bytesBeyondTheDeclaredSizeAreCutOff() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = try fixture.sealTagged(plaintext)
        let head = Data(plaintext.prefix(5))
        #expect(try fixture.decrypt(blob, plaintextDigests: sha256(head), declaredSize: 5) == head)
    }

    @Test func oversizedCiphertextIsRefused() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = Data(count: EncryptedFileDecryptor.maximumCiphertextSize + 1)
        #expect(throws: EncryptedFileError.tooLarge(byteCount: blob.count)) { try fixture.decrypt(blob) }
    }

    // MARK: CBC

    @Test func cbcWithMatchingDigestRoundTrips() throws {
        let fixture = try Fixture(cipher: .aes256CBC)
        let blob = try fixture.sealCBC(plaintext)
        #expect(try fixture.decrypt(blob, plaintextDigests: sha256(plaintext)) == plaintext)
        #expect(try fixture.decrypt(blob, encryptedDigests: sha256(blob)) == plaintext)
    }

    @Test func cbcWithoutDigestIsRejected() throws {
        let fixture = try Fixture(cipher: .aes256CBC)
        let blob = try fixture.sealCBC(plaintext)
        #expect(throws: EncryptedFileError.unauthenticated) { try fixture.decrypt(blob) }
    }

    @Test func cbcWithWrongKeyIsRejected() throws {
        let blob = try Fixture(cipher: .aes256CBC).sealCBC(plaintext)
        let wrong = try Fixture(cipher: .aes256CBC)
        #expect(throws: EncryptedFileError.self) {
            try wrong.decrypt(blob, plaintextDigests: sha256(plaintext))
        }
    }

    @Test func cbcWithPartialBlockIsMalformed() throws {
        let fixture = try Fixture(cipher: .aes256CBC)
        let blob = try fixture.sealCBC(plaintext).dropLast()
        #expect(throws: EncryptedFileError.malformedCiphertext) {
            try fixture.decrypt(Data(blob), plaintextDigests: sha256(plaintext))
        }
    }

    // MARK: SharedFile entry point

    @Test func decryptsAgainstTheSharedFileMetadata() throws {
        let fixture = try Fixture(cipher: .aes256GCM)
        let blob = try fixture.sealTagless(plaintext)
        let source = EncryptedFileSource(
            cipher: .aes256GCM,
            keyBase64: fixture.key.base64EncodedString(),
            ivBase64: fixture.iv.base64EncodedString(),
            digests: FileDigests(),
            sources: [URL(string: "https://files.waddle.test/blob")!]
        )
        let file = SharedFile(
            url: URL(string: "https://files.waddle.test/blob")!,
            name: "summit.txt",
            size: plaintext.count,
            digests: sha256(plaintext),
            encrypted: source
        )
        #expect(try EncryptedFileDecryptor.decrypt(blob, source: source, file: file) == plaintext)
    }

    @Test func localFileNameIsOneSafePathComponent() {
        let url = URL(string: "https://files.waddle.test/blob")!
        #expect(SharedFile(url: url, name: "../../etc/passwd").localFileName == ".._.._etc_passwd")
        #expect(SharedFile(url: url, name: "summit.jpg").localFileName == "summit.jpg")
        #expect(SharedFile(url: url, name: " .. ").localFileName == "attachment")
        #expect(SharedFile(url: url, name: "").localFileName == "attachment")
        #expect(SharedFile(url: url, name: String(repeating: "é", count: 300)).localFileName.utf8.count == 200)
    }

    private func sha256(_ data: Data) -> FileDigests {
        FileDigests([.sha256: Data(SHA256.hash(data: data))])
    }
}

/// Random key material for one cipher, plus sender-side encryption.
private struct Fixture {
    let cipher: FileCipher
    let key: Data
    let iv: Data

    init(cipher: FileCipher) throws {
        self.cipher = cipher
        key = SymmetricKey(size: SymmetricKeySize(bitCount: cipher.keyByteCount * 8)).withUnsafeBytes { Data($0) }
        iv = Data((0..<cipher.ivByteCount).map { _ in UInt8.random(in: .min ... .max) })
    }

    func sealTagged(_ plaintext: Data) throws -> Data {
        let box = try AES.GCM.seal(plaintext, using: SymmetricKey(data: key), nonce: AES.GCM.Nonce(data: iv))
        return box.ciphertext + box.tag
    }

    func sealTagless(_ plaintext: Data) throws -> Data {
        let box = try AES.GCM.seal(plaintext, using: SymmetricKey(data: key), nonce: AES.GCM.Nonce(data: iv))
        return box.ciphertext
    }

    func sealCBC(_ plaintext: Data) throws -> Data {
        try AES._CBC.encrypt(plaintext, using: SymmetricKey(data: key), iv: AES._CBC.IV(ivBytes: iv))
    }

    func decrypt(
        _ blob: Data,
        encryptedDigests: FileDigests = FileDigests(),
        plaintextDigests: FileDigests = FileDigests(),
        declaredSize: Int? = nil
    ) throws -> Data {
        try EncryptedFileDecryptor.decrypt(
            blob,
            key: EncryptedFileKey(cipher: cipher, key: key, iv: iv),
            encryptedDigests: encryptedDigests,
            plaintextDigests: plaintextDigests,
            declaredSize: declaredSize
        )
    }
}
