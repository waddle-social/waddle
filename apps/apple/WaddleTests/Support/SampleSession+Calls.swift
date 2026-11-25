import Foundation
@testable import Waddle
@testable import WaddleKit

@MainActor
extension SampleSession {
    /// Ana on the device a call's session is set up with.
    static let anaOnThisMac = AccountIdentity(jid: ana.jid, nick: ana.nick, resource: "mac")

    /// Answers a call from `peer`, leaving the slot with a session whose
    /// media has not connected: what a stage shows first.
    func answerCall(from peer: BareJID, video: Bool, sid: String = "sample-call") async {
        guard let from = peer.with(resource: "web") else { return }
        let media = CallMedia(audio: true, video: video)
        let join = CallJoin(url: "wss://sfu.waddle.test", room: "dm", identity: "\(Self.ana.jid)/mac", token: "")
        coordinator.calls.handle(CallEvent(from: from, sid: sid, kind: .propose(media)))
        _ = await coordinator.calls.acceptIncoming()
        coordinator.calls.handle(CallEvent(from: from, sid: sid, kind: .sessionInitiate(join: join, media: media)))
    }
}
