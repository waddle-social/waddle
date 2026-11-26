import CryptoKit
import Foundation
import WaddleKit
import XCTest
@testable import Waddle

/// Mints the tokens Waddle's server would issue, and administers rooms.
struct TestSFU {
    /// The SFU named by `WADDLE_LIVEKIT_TEST_URL`; a test that needs one is skipped without it.
    static func fromEnvironment() throws -> TestSFU {
        let environment = ProcessInfo.processInfo.environment
        guard let url = environment["WADDLE_LIVEKIT_TEST_URL"], !url.isEmpty else {
            throw XCTSkip("WADDLE_LIVEKIT_TEST_URL is not set")
        }
        return TestSFU(
            url: url,
            key: environment["WADDLE_LIVEKIT_TEST_KEY"] ?? "devkey",
            secret: environment["WADDLE_LIVEKIT_TEST_SECRET"] ?? "secret"
        )
    }

    let url: String
    let key: String
    let secret: String

    func join(room: String, identity: String) -> CallJoin {
        CallJoin(url: url, room: room, identity: identity, token: token(subject: identity, grant: ["roomJoin": true, "room": room]))
    }

    func deleteRoom(_ room: String) async throws {
        var components = URLComponents(string: url)!
        components.scheme = components.scheme == "wss" ? "https" : "http"
        components.path = "/twirp/livekit.RoomService/DeleteRoom"
        var request = URLRequest(url: components.url!)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(token(subject: "admin", grant: ["roomCreate": true]))", forHTTPHeaderField: "Authorization")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["room": room])
        let (_, response) = try await URLSession.shared.data(for: request)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
    }

    private func token(subject: String, grant: [String: Any]) -> String {
        let now = Int(Date().timeIntervalSince1970)
        let header = segment(["alg": "HS256", "typ": "JWT"])
        let payload = segment(["iss": key, "sub": subject, "nbf": now - 10, "exp": now + 600, "video": grant])
        let signature = HMAC<SHA256>.authenticationCode(for: Data("\(header).\(payload)".utf8), using: SymmetricKey(data: Data(secret.utf8)))
        return "\(header).\(payload).\(base64URL(Data(signature)))"
    }

    private func segment(_ object: [String: Any]) -> String {
        base64URL((try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data())
    }

    private func base64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
