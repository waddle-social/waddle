import Foundation

/// JIDs encode as their canonical string and are re-validated on decode, so
/// a persisted value is known-valid exactly like a parsed one.
extension BareJID: Codable {
    public init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(String.self)
        guard let parsed = BareJID(parsing: raw) else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "Invalid bare JID")
        }
        self = parsed
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(description)
    }
}

extension JID: Codable {
    public init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(String.self)
        guard let parsed = JID(parsing: raw) else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "Invalid JID")
        }
        self = parsed
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(description)
    }
}
