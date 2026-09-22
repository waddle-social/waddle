import Foundation

/// A sign-in provider the server offers (`/api/auth/providers`).
struct AuthProvider: Decodable, Identifiable, Hashable {
    let id: String
    let kind: String
    let displayName: String?

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case displayName = "display_name"
    }

    var title: String { displayName ?? id.capitalized }
}

/// An authenticated Waddle session (`/api/auth/session`). The session id is
/// the XMPP bearer credential.
struct AuthSession: Decodable, Equatable {
    let sessionID: String
    let userID: String
    let username: String
    let jid: String
    let xmppWebsocketURL: String
    let isExpired: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case userID = "user_id"
        case username
        case jid
        case xmppWebsocketURL = "xmpp_websocket_url"
        case isExpired = "is_expired"
    }
}

/// RFC 8628 device authorization start response.
struct DeviceAuthorization: Decodable, Equatable {
    let deviceCode: String
    let userCode: String
    let verificationURI: String?
    let verificationURIComplete: String?
    let interval: Int

    enum CodingKeys: String, CodingKey {
        case deviceCode = "device_code"
        case userCode = "user_code"
        case verificationURI = "verification_uri"
        case verificationURIComplete = "verification_uri_complete"
        case interval
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        deviceCode = try container.decode(String.self, forKey: .deviceCode)
        userCode = try container.decode(String.self, forKey: .userCode)
        verificationURI = try container.decodeIfPresent(String.self, forKey: .verificationURI)
        verificationURIComplete = try container.decodeIfPresent(String.self, forKey: .verificationURIComplete)
        interval = max(1, try container.decodeIfPresent(Int.self, forKey: .interval) ?? 5)
    }
}

enum DevicePollResult: Equatable {
    case pending
    case complete(sessionID: String)
}
