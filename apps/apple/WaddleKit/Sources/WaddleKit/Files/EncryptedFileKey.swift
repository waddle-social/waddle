import Foundation

/// Validated XEP-0448 key material: a supported cipher with a key and IV of
/// the lengths §5 requires.
public struct EncryptedFileKey: Sendable {
    public let cipher: FileCipher
    public let key: Data
    public let iv: Data

    public init(cipher: FileCipher, key: Data, iv: Data) throws {
        guard key.count == cipher.keyByteCount else {
            throw EncryptedFileError.wrongKeyLength(expected: cipher.keyByteCount, actual: key.count)
        }
        guard iv.count == cipher.ivByteCount else {
            throw EncryptedFileError.wrongIVLength(expected: cipher.ivByteCount, actual: iv.count)
        }
        self.cipher = cipher
        self.key = key
        self.iv = iv
    }

    public init(_ source: EncryptedFileSource) throws {
        guard let key = Data(base64Encoded: source.keyBase64),
              let iv = Data(base64Encoded: source.ivBase64)
        else { throw EncryptedFileError.malformedKeyMaterial }
        try self.init(cipher: source.cipher, key: key, iv: iv)
    }
}
