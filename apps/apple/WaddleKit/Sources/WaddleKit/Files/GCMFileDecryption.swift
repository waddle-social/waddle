import Crypto
import CryptoExtras
import Foundation

/// AES-GCM for XEP-0448. §5 recommends appending the 16-byte tag but does
/// not require it, since the file's digest travels in the message; both
/// layouts are accepted.
enum GCMFileDecryption {
    static let tagByteCount = 16

    static func decrypt(_ blob: Data, key: EncryptedFileKey, declaredSize: Int?) throws -> DecryptedBytes {
        if let plaintext = openTagged(blob, key: key) {
            return DecryptedBytes(plaintext: plaintext, isAuthenticated: true)
        }
        if expectsTag(blobByteCount: blob.count, declaredSize: declaredSize) {
            throw EncryptedFileError.authenticationFailed
        }
        return DecryptedBytes(
            plaintext: try decryptTagless(blob, key: key),
            isAuthenticated: false,
            isLayoutAmbiguous: declaredSize == nil && blob.count >= tagByteCount
        )
    }

    /// Ciphertext exactly one tag longer than the declared plaintext was
    /// sent tagged, so a failed tag is final rather than a tagless file.
    static func expectsTag(blobByteCount: Int, declaredSize: Int?) -> Bool {
        guard let declaredSize, blobByteCount >= tagByteCount else { return false }
        return blobByteCount - tagByteCount == declaredSize
    }

    /// The plaintext when the trailing 16 bytes are a valid tag, else nil.
    static func openTagged(_ blob: Data, key: EncryptedFileKey) -> Data? {
        guard blob.count >= tagByteCount else { return nil }
        do {
            let box = try AES.GCM.SealedBox(
                nonce: AES.GCM.Nonce(data: key.iv),
                ciphertext: blob.dropLast(tagByteCount),
                tag: blob.suffix(tagByteCount)
            )
            return try AES.GCM.open(box, using: SymmetricKey(data: key.key))
        } catch {
            return nil
        }
    }

    /// GCM without its tag is AES-CTR from J0 + 1, which for a 96-bit IV is
    /// IV ‖ 0x00000002 (NIST SP 800-38D §7.2).
    static func decryptTagless(_ blob: Data, key: EncryptedFileKey) throws -> Data {
        let counterBlock = key.iv + [0, 0, 0, 2]
        return try AES._CTR.decrypt(
            blob,
            using: SymmetricKey(data: key.key),
            nonce: AES._CTR.Nonce(nonceBytes: counterBlock)
        )
    }
}
