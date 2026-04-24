import Foundation

struct AuthProvider: Decodable, Identifiable, Hashable {
    let id: String
    let kind: String
    let displayName: String?

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case displayName = "display_name"
    }
}

struct WaddleSession: Decodable {
    let sessionID: String
    let userID: String
    let username: String
    let avatarURL: String?
    let xmppLocalpart: String
    let jid: String
    let xmppWebsocketURL: String
    let isExpired: Bool
    let expiresAt: String?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case userID = "user_id"
        case username
        case avatarURL = "avatar_url"
        case xmppLocalpart = "xmpp_localpart"
        case jid
        case xmppWebsocketURL = "xmpp_websocket_url"
        case isExpired = "is_expired"
        case expiresAt = "expires_at"
    }
}

struct DeviceStartResponse: Decodable {
    let deviceCode: String
    let userCode: String
    let verificationURI: String?
    let verificationURIComplete: String?
    let interval: Int
    let expiresIn: Int?

    enum CodingKeys: String, CodingKey {
        case deviceCode = "device_code"
        case userCode = "user_code"
        case verificationURI = "verification_uri"
        case verificationURIComplete = "verification_uri_complete"
        case interval
        case expiresIn = "expires_in"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        deviceCode = try container.decode(String.self, forKey: .deviceCode)
        userCode = try container.decode(String.self, forKey: .userCode)
        verificationURI = try container.decodeIfPresent(String.self, forKey: .verificationURI)
        verificationURIComplete = try container.decodeIfPresent(String.self, forKey: .verificationURIComplete)
        interval = try container.decodeIfPresent(Int.self, forKey: .interval) ?? 5
        expiresIn = try container.decodeIfPresent(Int.self, forKey: .expiresIn)
    }
}

struct DevicePollCompleteResponse: Decodable {
    let status: String
    let sessionID: String

    enum CodingKeys: String, CodingKey {
        case status
        case sessionID = "session_id"
    }
}

struct SpaceSummary: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
    let description: String?
}

struct ChannelSummary: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
    let description: String?
    let channelType: String?
    let position: Int?
    /// Full XMPP room JID (e.g. `{channelUUID}@muc.domain`).
    /// Set from XMPP discovery; not decoded from JSON.
    var roomJid: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case description
        case channelType = "channel_type"
        case position
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        description = try c.decodeIfPresent(String.self, forKey: .description)
        channelType = try c.decodeIfPresent(String.self, forKey: .channelType)
        position = try c.decodeIfPresent(Int.self, forKey: .position)
        roomJid = ""
    }

    init(id: String, roomJid: String = "", name: String, description: String? = nil, channelType: String? = nil, position: Int? = nil) {
        self.id = id
        self.roomJid = roomJid
        self.name = name
        self.description = description
        self.channelType = channelType
        self.position = position
    }
}

struct MemberSummary: Decodable, Identifiable, Hashable {
    let userID: String
    let username: String
    let avatarURL: String?
    let role: String
    let joinedAt: String

    var id: String { userID }

    enum CodingKeys: String, CodingKey {
        case userID = "user_id"
        case username
        case avatarURL = "avatar_url"
        case role
        case joinedAt = "joined_at"
    }
}

struct CreateSpaceRequest: Encodable {
    let name: String
    let description: String?
}
