import CryptoKit
import WaddleKit
import XCTest
@testable import Waddle

/// The LiveKit SDK against a real SFU. Skipped unless
/// `WADDLE_LIVEKIT_TEST_URL` names one, since CI has none; see the README
/// for the local `livekit-server --dev` recipe.
///
/// This is the compatibility check to repeat when either the SDK pin or the
/// deployed SFU version changes.
@MainActor
final class LiveKitCallMediaTests: XCTestCase {
    private var sfu: TestSFU!

    override func setUp() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let url = environment["WADDLE_LIVEKIT_TEST_URL"], !url.isEmpty else {
            throw XCTSkip("WADDLE_LIVEKIT_TEST_URL is not set")
        }
        sfu = TestSFU(
            url: url,
            key: environment["WADDLE_LIVEKIT_TEST_KEY"] ?? "devkey",
            secret: environment["WADDLE_LIVEKIT_TEST_SECRET"] ?? "secret"
        )
    }

    func testJoinsARoomAndLeaves() async throws {
        let media = LiveKitCallMediaController()
        let join = sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac")
        XCTAssertNil(join.grantDefect)

        try await media.connect(join: join, media: CallMedia(audio: false, video: false), iceServers: [])
        await media.disconnect()
        // A second call reuses the controller.
        try await media.connect(join: join, media: CallMedia(audio: false, video: false), iceServers: [])
        await media.disconnect()
    }

    func testJoinsWithAdvertisedIceServers() async throws {
        let media = LiveKitCallMediaController()
        let join = sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac")
        // What XEP-0215 would advertise, mapped the way the call store maps it.
        let advertised = WaddleKit.IceServer.servers(from: [
            ExternalService(kind: .stun, host: "127.0.0.1", port: 3478),
            ExternalService(kind: .turn, host: "127.0.0.1", port: 3478, transport: .udp, username: "u", password: "p"),
        ])

        try await media.connect(join: join, media: CallMedia(audio: false, video: false), iceServers: advertised)
        await media.disconnect()
    }

    func testATokenTheSFUDidNotIssueCannotJoin() async {
        let media = LiveKitCallMediaController()
        let forged = TestSFU(url: sfu.url, key: sfu.key, secret: "not-the-secret")
            .join(room: "waddle-test-\(UUID().uuidString)", identity: "mallory@waddle.test/mac")
        do {
            try await media.connect(join: forged, media: CallMedia(audio: false, video: false), iceServers: [])
            XCTFail("a forged token joined the room")
            await media.disconnect()
        } catch {
            XCTAssertFalse(error is CancellationError, "the SFU refused the join; nothing cancelled it")
        }
    }

    func testTheSFUClosingTheRoomIsReportedAsAFailure() async throws {
        let media = LiveKitCallMediaController()
        let room = "waddle-test-\(UUID().uuidString)"
        let dropped = expectation(description: "the media session reported its death")
        media.onFailure = { dropped.fulfill() }

        try await media.connect(
            join: sfu.join(room: room, identity: "alice@waddle.test/mac"),
            media: CallMedia(audio: false, video: false), iceServers: []
        )
        try await sfu.deleteRoom(room)
        await fulfillment(of: [dropped], timeout: 15)
        await media.disconnect()
    }

    func testLeavingIsNotReportedAsAFailure() async throws {
        let media = LiveKitCallMediaController()
        var failures = 0
        media.onFailure = { failures += 1 }

        try await media.connect(
            join: sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac"),
            media: CallMedia(audio: false, video: false), iceServers: []
        )
        await media.disconnect()
        try await Task.sleep(for: .seconds(1))
        XCTAssertEqual(failures, 0)
    }
}

/// Mints the tokens Waddle's server would issue, and administers rooms.
private struct TestSFU {
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
