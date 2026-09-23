/// Why a XEP-0448 file could not be opened.
public enum EncryptedFileError: Error, Hashable, Sendable {
    /// `<key/>` or `<iv/>` is not base64.
    case malformedKeyMaterial
    case wrongKeyLength(expected: Int, actual: Int)
    case wrongIVLength(expected: Int, actual: Int)
    /// The ciphertext exceeds `EncryptedFileDecryptor.maximumCiphertextSize`.
    case tooLarge(byteCount: Int)
    /// The ciphertext does not match the `<encrypted/>` digests.
    case ciphertextDigestMismatch
    /// The decrypted file does not match the `<file/>` digests.
    case plaintextDigestMismatch
    /// The GCM tag the ciphertext length implies did not verify: the key,
    /// IV or ciphertext is wrong.
    case authenticationFailed
    /// Decryption produced bytes nothing vouches for: no GCM tag and no
    /// matching digest.
    case unauthenticated
    /// CBC ciphertext that is not whole blocks or whose padding is invalid.
    case malformedCiphertext
}
