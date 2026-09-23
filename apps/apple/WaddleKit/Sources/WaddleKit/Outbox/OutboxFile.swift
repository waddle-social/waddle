import Foundation

/// The versioned JSON envelope of a saved outbox.
struct OutboxFile: Codable {
    static let currentVersion = 1

    let version: Int
    let entries: [PersistedOutbound]

    private struct Header: Decodable {
        let version: Int
    }

    enum DecodeError: Error, Equatable {
        case unknownVersion(Int)
        case unreadable
    }

    static func encode(_ entries: [PersistedOutbound]) throws -> Data {
        try JSONEncoder().encode(OutboxFile(version: currentVersion, entries: entries))
    }

    /// The version is checked first, so a file from another schema is
    /// reported as such rather than as corrupt.
    static func decode(_ data: Data) -> Result<[PersistedOutbound], DecodeError> {
        let decoder = JSONDecoder()
        guard let header = try? decoder.decode(Header.self, from: data) else {
            return .failure(.unreadable)
        }
        guard header.version == currentVersion else {
            return .failure(.unknownVersion(header.version))
        }
        guard let file = try? decoder.decode(OutboxFile.self, from: data) else {
            return .failure(.unreadable)
        }
        return .success(file.entries)
    }
}
