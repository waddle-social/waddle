import Crypto
import Foundation

/// A file sealed for XEP-0448: what to upload, and what the message says
/// about it. Each file gets a fresh AES-256-GCM key and IV.
public struct EncryptedUpload: Sendable {
    /// The bytes to upload: the ciphertext followed by its 16-byte GCM tag,
    /// the layout §5 recommends.
    public let ciphertext: Data
    /// SHA-256 of the file itself, for its XEP-0446 metadata (§2.1 requires
    /// a digest of the plaintext).
    public let plaintextDigests: FileDigests
    private let key: EncryptedFileKey

    public init(sealing plaintext: Data) throws {
        let symmetricKey = SymmetricKey(size: .bits256)
        let nonce = AES.GCM.Nonce()
        let box = try AES.GCM.seal(plaintext, using: symmetricKey, nonce: nonce)
        ciphertext = box.ciphertext + box.tag
        plaintextDigests = FileDigests([.sha256: DigestCheck.digest(of: plaintext, using: .sha256)])
        key = try EncryptedFileKey(
            cipher: .aes256GCM,
            key: symmetricKey.withUnsafeBytes { Data($0) },
            iv: Data(nonce)
        )
    }

    /// The `<encrypted/>` envelope once the ciphertext is stored at `url`.
    public func source(at url: URL) -> EncryptedFileSource {
        EncryptedFileSource(
            cipher: key.cipher,
            keyBase64: key.key.base64EncodedString(),
            ivBase64: key.iv.base64EncodedString(),
            digests: FileDigests([.sha256: DigestCheck.digest(of: ciphertext, using: .sha256)]),
            sources: [url]
        )
    }
}
