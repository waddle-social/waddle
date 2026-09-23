import Crypto
import CryptoExtras
import Foundation

/// AES-256-CBC with PKCS#7 padding for XEP-0448. CBC carries no
/// authentication; the caller must match a digest.
enum CBCFileDecryption {
    static let blockByteCount = 16

    static func decrypt(_ blob: Data, key: EncryptedFileKey) throws -> DecryptedBytes {
        guard !blob.isEmpty, blob.count % blockByteCount == 0 else {
            throw EncryptedFileError.malformedCiphertext
        }
        do {
            let plaintext = try AES._CBC.decrypt(
                blob,
                using: SymmetricKey(data: key.key),
                iv: AES._CBC.IV(ivBytes: key.iv)
            )
            return DecryptedBytes(plaintext: plaintext, isAuthenticated: false)
        } catch {
            throw EncryptedFileError.malformedCiphertext
        }
    }
}
