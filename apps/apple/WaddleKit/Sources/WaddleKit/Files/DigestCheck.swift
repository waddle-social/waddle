import Crypto
import Foundation

/// Outcome of checking bytes against XEP-0300 digests.
public enum DigestCheck: Hashable, Sendable {
    /// Every declared digest matched.
    case matched
    /// At least one declared digest did not match.
    case mismatched
    /// No digest was declared.
    case noSupportedDigest

    public init(_ data: Data, against digests: FileDigests) {
        guard !digests.isEmpty else {
            self = .noSupportedDigest
            return
        }
        let allMatch = digests.values.allSatisfy { algorithm, expected in
            Self.digest(of: data, using: algorithm) == expected
        }
        self = allMatch ? .matched : .mismatched
    }

    static func digest(of data: Data, using algorithm: HashAlgorithm) -> Data {
        switch algorithm {
        case .sha256: return Data(SHA256.hash(data: data))
        case .sha512: return Data(SHA512.hash(data: data))
        }
    }
}
