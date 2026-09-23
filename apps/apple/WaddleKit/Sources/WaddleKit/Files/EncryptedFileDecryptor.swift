import Foundation

/// Opens XEP-0448 ciphertext fetched from an `<encrypted/>` source.
public enum EncryptedFileDecryptor {
    /// Larger downloads are refused before decryption.
    public static let maximumCiphertextSize = 100 * 1024 * 1024

    /// Checks the ciphertext digests, decrypts, cuts the result to
    /// `declaredSize` (§3.2), then checks the plaintext digests. Bytes that
    /// neither a GCM tag nor a matching digest vouch for are refused.
    public static func decrypt(
        _ blob: Data,
        key: EncryptedFileKey,
        encryptedDigests: FileDigests,
        plaintextDigests: FileDigests,
        declaredSize: Int?
    ) throws -> Data {
        guard blob.count <= maximumCiphertextSize else {
            throw EncryptedFileError.tooLarge(byteCount: blob.count)
        }
        let ciphertextCheck = DigestCheck(blob, against: encryptedDigests)
        guard ciphertextCheck != .mismatched else { throw EncryptedFileError.ciphertextDigestMismatch }
        let decrypted = try decryptBytes(blob, key: key, declaredSize: declaredSize)
        let plaintext = truncated(decrypted.plaintext, to: declaredSize)
        let plaintextCheck = DigestCheck(plaintext, against: plaintextDigests)
        guard plaintextCheck != .mismatched else { throw EncryptedFileError.plaintextDigestMismatch }
        // A matching ciphertext digest vouches for the bytes, not for how
        // they were read: with an ambiguous GCM layout the former tag may
        // have been decrypted as data, so only the plaintext digest counts.
        let ciphertextVouches = ciphertextCheck == .matched && !decrypted.isLayoutAmbiguous
        guard decrypted.isAuthenticated || ciphertextVouches || plaintextCheck == .matched else {
            throw EncryptedFileError.unauthenticated
        }
        return plaintext
    }

    /// `blob` fetched from `source`, checked against `file`'s metadata.
    public static func decrypt(_ blob: Data, source: EncryptedFileSource, file: SharedFile) throws -> Data {
        try decrypt(
            blob,
            key: EncryptedFileKey(source),
            encryptedDigests: source.digests,
            plaintextDigests: file.digests,
            declaredSize: file.size
        )
    }

    static func decryptBytes(_ blob: Data, key: EncryptedFileKey, declaredSize: Int?) throws -> DecryptedBytes {
        switch key.cipher {
        case .aes128GCM, .aes256GCM:
            return try GCMFileDecryption.decrypt(blob, key: key, declaredSize: declaredSize)
        case .aes256CBC:
            return try CBCFileDecryption.decrypt(blob, key: key)
        }
    }

    /// XEP-0448 §3.2: bytes beyond the declared size are cut off.
    static func truncated(_ plaintext: Data, to declaredSize: Int?) -> Data {
        guard let declaredSize, declaredSize >= 0, plaintext.count > declaredSize else { return plaintext }
        return Data(plaintext.prefix(declaredSize))
    }
}
