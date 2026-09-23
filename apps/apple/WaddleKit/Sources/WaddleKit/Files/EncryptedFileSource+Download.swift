import Foundation

extension EncryptedFileSource {
    /// The first `https` source: ciphertext is only fetched over TLS.
    public var downloadURL: URL? {
        sources.first { $0.scheme?.lowercased() == "https" }
    }
}
