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

private struct PublicWaddlesResponse: Decodable {
    let waddles: [WaddleSummary]
    let total: Int
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

    func listPublicWaddles(sessionID: String, query: String?) async throws -> [WaddleSummary] {
        var items = [
            URLQueryItem(name: "session_id", value: sessionID),
            URLQueryItem(name: "limit", value: "100"),
        ]
        let trimmed = query?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !trimmed.isEmpty {
            items.append(URLQueryItem(name: "query", value: trimmed))
        }

        let (data, _) = try await send(path: "/v1/waddles/public", queryItems: items)
        return try decoder.decode(PublicWaddlesResponse.self, from: data).waddles
    }

    func joinWaddle(sessionID: String, waddleID: String) async throws {
        _ = try await send(
            path: "/v1/waddles/\(waddleID)/join",
            method: "POST",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)]
        )
    }

    func createWaddle(sessionID: String, name: String, description: String?, isPublic: Bool) async throws -> WaddleSummary {
        let body = try encoder.encode(
            CreateWaddleRequest(
                name: name,
                description: description?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
                isPublic: isPublic
            )
        )
        let (data, _) = try await send(
            path: "/v1/waddles",
            method: "POST",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
        return try decoder.decode(WaddleSummary.self, from: data)
    }

    func updateWaddle(sessionID: String, waddleID: String, name: String, description: String?) async throws {
        let payload: [String: String?] = ["name": name, "description": description]
        let body = try JSONEncoder().encode(payload)
        let _ = try await send(
            path: "/v1/waddles/\(waddleID)",
            method: "POST",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func deleteWaddle(sessionID: String, waddleID: String) async throws {
        let _ = try await send(
            path: "/v1/waddles/\(waddleID)",
            method: "DELETE",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            treatStatusesAsNil: [204]
        )
    }

    func updateChannel(sessionID: String, waddleID: String, channelID: String, name: String, description: String?, position: Int) async throws {
        let payload: [String: Any] = ["name": name, "description": description as Any, "position": position]
        let body = try JSONSerialization.data(withJSONObject: payload)
        let _ = try await send(
            path: "/v1/waddles/\(waddleID)/channels/\(channelID)",
            method: "PATCH",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func listMembers(sessionID: String, waddleID: String) async throws -> [MemberSummary] {
        let (data, _) = try await send(
            path: "/v1/waddles/\(waddleID)/members",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)]
        )
        return try decoder.decode(MembersResponse.self, from: data).members
    }

    func addMember(sessionID: String, waddleID: String, userID: String, role: String = "member") async throws {
        let body = try JSONEncoder().encode(["user_id": userID, "role": role])
        let _ = try await send(
            path: "/v1/waddles/\(waddleID)/members",
            method: "POST",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            body: body
        )
    }

    func removeMember(sessionID: String, waddleID: String, userID: String) async throws {
        let _ = try await send(
            path: "/v1/waddles/\(waddleID)/members/\(userID)",
            method: "DELETE",
            queryItems: [URLQueryItem(name: "session_id", value: sessionID)],
            treatStatusesAsNil: [204]
        )
    }

    func updateMemberRole(sessionID: String, waddleID: String, userID: String, role: String) async throws {
        let body = try JSONEncoder().encode(["role": role])
        let _ = try await send(
            path: "/v1/waddles/\(waddleID)/members/\(userID)",
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
