import Foundation

/// XEP-0300 hash algorithms Waddle verifies.
public enum HashAlgorithm: String, Hashable, Sendable, CaseIterable, Codable, CodingKeyRepresentable {
    case sha256 = "sha-256"
    case sha512 = "sha-512"
}

/// XEP-0300 digests of one file, by algorithm.
public struct FileDigests: Hashable, Sendable, Codable {
    public let values: [HashAlgorithm: Data]

    public init(_ values: [HashAlgorithm: Data] = [:]) {
        self.values = values
    }

    /// Wire `<hash algo='…'>base64</hash>` pairs. Unsupported algorithms and
    /// digests that are not base64 are skipped: an absent digest never
    /// authenticates anything, so skipping fails closed. The first digest
    /// per algorithm wins.
    public init(xep0300 hashes: [(algorithm: String, base64: String)]) {
        var values: [HashAlgorithm: Data] = [:]
        for hash in hashes {
            guard let algorithm = HashAlgorithm(rawValue: hash.algorithm),
                  values[algorithm] == nil,
                  let digest = Data(base64Encoded: hash.base64)
            else { continue }
            values[algorithm] = digest
        }
        self.values = values
    }

    /// Wire pairs, ordered by algorithm name.
    public var xep0300: [(algorithm: String, base64: String)] {
        values
            .map { (algorithm: $0.key.rawValue, base64: $0.value.base64EncodedString()) }
            .sorted { $0.algorithm < $1.algorithm }
    }

    public var isEmpty: Bool { values.isEmpty }
}
