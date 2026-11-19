import Foundation
import WaddleKit

@testable import WaddleKitTestSupport

let callOwn = JID(parsing: "alice@waddle.test/waddle-apple-1")!
let callPeer = JID(parsing: "bob@waddle.test/phone")!
let callAudio = CallMedia(audio: true, video: false)
let callVideo = CallMedia(audio: true, video: true)
let callJoin = CallJoin(
    url: "wss://livekit.waddle.test", room: "dm-room", identity: "alice@waddle.test/waddle-apple-1",
    token: callToken(#"{"sub":"alice@waddle.test/waddle-apple-1","video":{"roomJoin":true,"room":"dm-room"}}"#))

@MainActor final class CallFixture {
    let port = FakePort()
    var own: JID? = callOwn
    let sid: () -> String
    let ringTimeout: Duration
    let sessionTimeout: Duration
    let unansweredTimeout: Duration
    let media: RecordingCallMedia?
    var now = ContinuousClock.now
    var reports: [CallLifecycleReport] = []
    init(
        sid: @escaping () -> String = { "c1" }, ringTimeout: Duration = .seconds(45), sessionTimeout: Duration = .seconds(45),
        unansweredTimeout: Duration = .seconds(60), media: RecordingCallMedia? = nil
    ) {
        self.sid = sid
        self.ringTimeout = ringTimeout
        self.sessionTimeout = sessionTimeout
        self.unansweredTimeout = unansweredTimeout
        self.media = media
    }
    lazy var store = CallStore(
        port: port, ownJID: { [weak self] in self?.own }, ownBareJID: callOwn.bare, media: media, ringTimeout: ringTimeout,
        sessionTimeout: sessionTimeout, unansweredTimeout: unansweredTimeout,
        now: { [weak self] in self?.now ?? ContinuousClock.now },
        report: { [weak self] in self?.reports.append($0) }, newSid: sid)
    func event(_ kind: CallEvent.Kind, sid: String = "c1", from: JID = callPeer) -> CallEvent {
        CallEvent(from: from, sid: sid, kind: kind)
    }
}

/// Literal payloads keep grant expectations independent of the validator.
func callToken(_ payload: String) -> String {
    func encode(_ text: String) -> String {
        Data(text.utf8).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
    return "\(encode(#"{"alg":"HS256","typ":"JWT"}"#)).\(encode(payload)).signature"
}
