import Foundation

enum WaddleAPIError: LocalizedError {
    case invalidServerURL
    case invalidResponse
    case server(statusCode: Int, message: String)

    var errorDescription: String? {
        switch self {
        case .invalidServerURL:
            return "Invalid server URL."
        case .invalidResponse:
            return "Invalid response from server."
        case let .server(statusCode, message):
            if message.isEmpty {
                return "Request failed (\(statusCode))."
            }
            return "Request failed (\(statusCode)): \(message)"
        }
    }
}

enum DevicePollResult {
    case pending
    case complete(DevicePollCompleteResponse)
}

private struct APIErrorResponse: Decodable {
    let message: String?
    let error: String?
}

private struct PollStatusResponse: Decodable {
    let status: String
}

private struct MembersResponse: Decodable {
    let members: [MemberSummary]
}

private struct ProviderStartRequest: Encodable {
    let provider: String
}

private struct DevicePollRequest: Encodable {
    let deviceCode: String

    enum CodingKeys: String, CodingKey {
        case deviceCode = "device_code"
    }
}

private struct SessionLogoutRequest: Encodable {
    let sessionID: String

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
    }
}

final class WaddleAPIClient {
    private let baseURL: URL
    private let decoder: JSONDecoder
    private let encoder: JSONEncoder

    init(serverURL: URL) {
        self.baseURL = serverURL
        self.decoder = JSONDecoder()
        self.encoder = JSONEncoder()
    }

    func providers() async throws -> [AuthProvider] {
        let (data, _) = try await send(path: "/api/auth/providers")
        return try decoder.decode([AuthProvider].self, from: data)
    }

    func startDeviceAuth(providerID: String) async throws -> DeviceStartResponse {
        let body = try encoder.encode(ProviderStartRequest(provider: providerID))
        let (data, _) = try await send(path: "/api/auth/device/start", method: "POST", body: body)
        return try decoder.decode(DeviceStartResponse.self, from: data)
    }

    func pollDeviceAuth(deviceCode: String) async throws -> DevicePollResult {
        let body = try encoder.encode(DevicePollRequest(deviceCode: deviceCode))
        let (data, _) = try await send(path: "/api/auth/device/poll", method: "POST", body: body)
        let status = try decoder.decode(PollStatusResponse.self, from: data)
        if status.status == "complete" {
            return .complete(try decoder.decode(DevicePollCompleteResponse.self, from: data))
        }
        return .pending
    }

    func session(sessionID: String?) async throws -> WaddleSession? {
        var query: [URLQueryItem] = []
        if let sessionID {
            query.append(URLQueryItem(name: "session_id", value: sessionID))
        }

        let (data, response) = try await send(
            path: "/api/auth/session",
            queryItems: query,
            treatStatusesAsNil: [401, 404]
        )

        guard let response else {
            return nil
        }

        if response.statusCode == 401 || response.statusCode == 404 {
            return nil
        }
        return try decoder.decode(WaddleSession.self, from: data)
    }

    func logout(sessionID: String?) async throws {
        let body: Data?
        if let sessionID {
            body = try encoder.encode(SessionLogoutRequest(sessionID: sessionID))
        } else {
            body = nil
        }

        _ = try await send(path: "/api/auth/logout", method: "POST", body: body)
    }

    func createSpace(sessionID: String, name: String, description: String?) async throws {
        let body = try encoder.encode(
            CreateWaddleRequest(
                name: name,
                description: description?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
                isPublic: false
            )
        )
        let _ = try await send(
            path: "/v1/space",
            method: "POST",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func updateSpace(sessionID: String, name: String, description: String?) async throws {
        let payload: [String: String?] = ["name": name, "description": description]
        let body = try JSONEncoder().encode(payload)
        let _ = try await send(
            path: "/v1/space",
            method: "PATCH",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func deleteSpace(sessionID: String) async throws {
        let _ = try await send(
            path: "/v1/space",
            method: "DELETE",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            treatStatusesAsNil: [204]
        )
    }

    func updateChannel(sessionID: String, channelID: String, name: String, description: String?, position: Int) async throws {
        let payload: [String: Any] = ["name": name, "description": description as Any, "position": position]
        let body = try JSONSerialization.data(withJSONObject: payload)
        let _ = try await send(
            path: "/v1/channels/\(channelID)",
            method: "PATCH",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func listMembers(sessionID: String) async throws -> [MemberSummary] {
        let (data, _) = try await send(
            path: "/v1/space/members",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)]
        )
        return try decoder.decode(MembersResponse.self, from: data).members
    }

    func addMember(sessionID: String, userID: String, role: String = "member") async throws {
        let body = try JSONEncoder().encode(["user_id": userID, "role": role])
        let _ = try await send(
            path: "/v1/space/members",
            method: "POST",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func removeMember(sessionID: String, userID: String) async throws {
        let _ = try await send(
            path: "/v1/space/members/\(userID)",
            method: "DELETE",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            treatStatusesAsNil: [204]
        )
    }

    func updateMemberRole(sessionID: String, userID: String, role: String) async throws {
        let body = try JSONEncoder().encode(["role": role])
        let _ = try await send(
            path: "/v1/space/members/\(userID)",
            method: "PATCH",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func searchUsers(sessionID: String, query: String) async throws -> [MemberSummary] {
        let (data, _) = try await send(
            path: "/api/users/search",
            queryItems: [
                URLQueryItem(name: "session_id", value: sessionID),
                URLQueryItem(name: "q", value: query),
            ]
        )
        return try decoder.decode([MemberSummary].self, from: data)
    }

    private func send(
        path: String,
        method: String = "GET",
        queryItems: [URLQueryItem] = [],
        body: Data? = nil,
        treatStatusesAsNil: Set<Int> = []
    ) async throws -> (Data, HTTPURLResponse?) {
        guard var components = URLComponents(
            url: baseURL.appending(path: path),
            resolvingAgainstBaseURL: false
        ) else {
            throw WaddleAPIError.invalidServerURL
        }
        if !queryItems.isEmpty {
            components.queryItems = queryItems
        }
        guard let url = components.url else {
            throw WaddleAPIError.invalidServerURL
        }

        var request = URLRequest(url: url)
        request.httpMethod = method
        request.httpBody = body
        if body != nil {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw WaddleAPIError.invalidResponse
        }

        if (200..<300).contains(http.statusCode) || treatStatusesAsNil.contains(http.statusCode) {
            return (data, http)
        }

        let apiError = try? decoder.decode(APIErrorResponse.self, from: data)
        let detail = apiError?.message ?? apiError?.error ?? ""
        throw WaddleAPIError.server(statusCode: http.statusCode, message: detail)
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
