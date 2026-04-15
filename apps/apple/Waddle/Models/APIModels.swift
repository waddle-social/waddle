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
    let verificationURI: String
    let verificationURIComplete: String
    let interval: Int
    let expiresIn: Int

    enum CodingKeys: String, CodingKey {
        case deviceCode = "device_code"
        case userCode = "user_code"
        case verificationURI = "verification_uri"
        case verificationURIComplete = "verification_uri_complete"
        case interval
        case expiresIn = "expires_in"
    }
}

struct DevicePollCompleteResponse: Decodable {
    let status: String
    let sessionID: String
    let userID: String
    let username: String
    let providerID: String
    let jid: String
    let xmppHost: String
    let xmppPort: Int

    enum CodingKeys: String, CodingKey {
        case status
        case sessionID = "session_id"
        case userID = "user_id"
        case username
        case providerID = "provider_id"
        case jid
        case xmppHost = "xmpp_host"
        case xmppPort = "xmpp_port"
    }
}

struct WaddleSummary: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
    let description: String?
    let ownerUserID: String?
    let iconURL: String?
    let isPublic: Bool?
    let role: String?
    let createdAt: String?
    let updatedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case description
        case ownerUserID = "owner_user_id"
        case iconURL = "icon_url"
        case isPublic = "is_public"
        case role
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct CreateWaddleRequest: Encodable {
    let name: String
    let description: String?
    let isPublic: Bool

    enum CodingKeys: String, CodingKey {
        case name
        case description
        case isPublic = "is_public"
    }
}
