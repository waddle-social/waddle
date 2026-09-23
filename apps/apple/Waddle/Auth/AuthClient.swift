import Foundation

enum AuthClientError: LocalizedError {
    case invalidServerURL
    case invalidResponse
    case server(statusCode: Int, message: String)

    var errorDescription: String? {
        switch self {
        case .invalidServerURL:
            return "That server address isn't valid."
        case .invalidResponse:
            return "The server sent an unexpected response."
        case let .server(statusCode, message):
            return message.isEmpty ? "Sign-in failed (\(statusCode))." : message
        }
    }
}

/// The device-authorization endpoints. Everything after sign-in is XMPP;
/// this client carries no chat data.
struct AuthClient {
    let baseURL: URL
    private let session: URLSession = .shared

    func providers() async throws -> [AuthProvider] {
        let (data, _) = try await request("/api/auth/providers")
        return try JSONDecoder().decode([AuthProvider].self, from: data)
    }

    func startDeviceAuthorization(provider: AuthProvider) async throws -> DeviceAuthorization {
        let body = try JSONEncoder().encode(["provider": provider.id])
        let (data, _) = try await request("/api/auth/device/start", method: "POST", body: body)
        return try JSONDecoder().decode(DeviceAuthorization.self, from: data)
    }

    func poll(_ authorization: DeviceAuthorization) async throws -> DevicePollResult {
        struct Response: Decodable {
            let status: String
            let sessionID: String?
            enum CodingKeys: String, CodingKey {
                case status
                case sessionID = "session_id"
            }
        }
        let body = try JSONEncoder().encode(["device_code": authorization.deviceCode])
        let (data, _) = try await request("/api/auth/device/poll", method: "POST", body: body)
        let response = try JSONDecoder().decode(Response.self, from: data)
        if response.status == "complete", let sessionID = response.sessionID {
            return .complete(sessionID: sessionID)
        }
        return .pending
    }

    /// The session for `sessionID`, or nil when it no longer exists.
    func session(_ sessionID: String) async throws -> AuthSession? {
        let (data, status) = try await request(
            "/api/auth/session",
            query: [URLQueryItem(name: "session_id", value: sessionID)],
            acceptedFailures: [401, 404]
        )
        guard status != 401, status != 404 else { return nil }
        let loaded = try JSONDecoder().decode(AuthSession.self, from: data)
        return loaded.isExpired ? nil : loaded
    }

    func logout(_ sessionID: String) async throws {
        let body = try JSONEncoder().encode(["session_id": sessionID])
        _ = try await request("/api/auth/logout", method: "POST", body: body)
    }

    /// The page the user approves the device code on. Only an http(s) page
    /// is ever opened, whatever the server answered.
    func verificationURL(for authorization: DeviceAuthorization) -> URL? {
        if let url = Self.webPage(authorization.verificationURIComplete) {
            return url
        }
        let base = Self.webPage(authorization.verificationURI)
            ?? baseURL.appending(path: "/api/auth/device/verify")
        var components = URLComponents(url: base, resolvingAgainstBaseURL: false)
        components?.queryItems = [URLQueryItem(name: "code", value: authorization.userCode)]
        return components?.url
    }

    private static func webPage(_ raw: String?) -> URL? {
        guard let raw, let url = URL(string: raw),
              let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http",
              url.host?.isEmpty == false
        else { return nil }
        return url
    }

    /// The XMPP WebSocket the session id is presented to. It must be TLS
    /// (`wss`), except a plain `ws` endpoint for a plain-http server, which
    /// `ServerSettings` only allows on loopback, so the bearer credential
    /// never crosses the network unencrypted.
    static func xmppWebSocketURL(_ raw: String, server: URL) -> URL? {
        guard let url = URL(string: raw), url.host?.isEmpty == false else { return nil }
        switch url.scheme?.lowercased() {
        case "wss": return url
        case "ws": return server.scheme?.lowercased() == "http" ? url : nil
        default: return nil
        }
    }

    private func request(
        _ path: String,
        method: String = "GET",
        query: [URLQueryItem] = [],
        body: Data? = nil,
        acceptedFailures: Set<Int> = []
    ) async throws -> (Data, Int) {
        guard var components = URLComponents(url: baseURL.appending(path: path), resolvingAgainstBaseURL: false) else {
            throw AuthClientError.invalidServerURL
        }
        if !query.isEmpty {
            components.queryItems = query
        }
        guard let url = components.url else { throw AuthClientError.invalidServerURL }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.httpBody = body
        if body != nil {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw AuthClientError.invalidResponse }
        if (200..<300).contains(http.statusCode) || acceptedFailures.contains(http.statusCode) {
            return (data, http.statusCode)
        }
        struct ErrorBody: Decodable {
            let message: String?
            let error: String?
        }
        let detail = (try? JSONDecoder().decode(ErrorBody.self, from: data)).flatMap { $0.message ?? $0.error } ?? ""
        throw AuthClientError.server(statusCode: http.statusCode, message: detail)
    }
}
