import WaddleKit

@testable import WaddleKitTestSupport

let callOwn = JID(parsing: "alice@waddle.test/waddle-apple-1")!
let callPeer = JID(parsing: "bob@waddle.test/phone")!
let callAudio = CallMedia(audio: true, video: false)
let callVideo = CallMedia(audio: true, video: true)
let callJoin = CallJoin(url: "wss://livekit.waddle.test", room: "dm-room", identity: "alice@waddle.test/waddle-apple-1", token: "jwt")

@MainActor final class CallFixture {
    let port = FakePort()
    var own: JID? = callOwn
    let sid: () -> String
    let ringTimeout: Duration
    let sessionTimeout: Duration
    init(sid: @escaping () -> String = { "c1" }, ringTimeout: Duration = .seconds(45), sessionTimeout: Duration = .seconds(45)) {
        self.sid = sid
        self.ringTimeout = ringTimeout
        self.sessionTimeout = sessionTimeout
    }
    lazy var store = CallStore(
        port: port, ownJID: { [weak self] in self?.own }, ownBareJID: callOwn.bare, ringTimeout: ringTimeout,
        sessionTimeout: sessionTimeout, newSid: sid)
    func event(_ kind: CallEvent.Kind, sid: String = "c1", from: JID = callPeer) -> CallEvent {
        CallEvent(from: from, to: nil, sid: sid, kind: kind)
    }
}
