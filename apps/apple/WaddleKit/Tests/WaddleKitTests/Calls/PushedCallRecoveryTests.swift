import Testing
import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Pushed call recovery") struct PushedCallRecoveryTests {
    @Test func expiredPushLeavesALaterStreamProposalItsOwnRingWindow() {
        let f = CallFixture()
        var tracker = SystemCallTracker()
        let pushed = tracker.pushed(
            sid: "c1", conversation: callPeer.bare, isVideo: false,
            isExpired: false, isFinished: f.store.isFinished(sid: "c1"), state: f.store.state)
        #expect(pushed.verdict == .awaiting)
        #expect(tracker.pushExpired(sid: "c1").count == 1)

        f.store.handle(f.event(.propose(callAudio)))

        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(tracker.update(state: f.store.state, isMediaConnected: false).count == 1)
        #expect(tracker.managedCall?.sid == "c1")
        f.store.clear()
    }

    @Test func losingTheSlotEndsThePushedScreenAndRejectsTheLaterProposal() async {
        let f = CallFixture(sid: { "other" })
        var tracker = SystemCallTracker()
        _ = tracker.pushed(
            sid: "c1", conversation: callPeer.bare, isVideo: false,
            isExpired: false, isFinished: f.store.isFinished(sid: "c1"), state: f.store.state)
        let pushedID = tracker.managedCall!.id
        #expect(await f.store.startCall(to: BareJID(parsing: "charlie@waddle.test")!, media: callAudio))
        let busyState = f.store.state
        let updates = tracker.update(state: busyState, isMediaConnected: false)
        #expect(updates.contains(.end(pushedID, .unanswered)))
        #expect(tracker.pushExpired(sid: "c1").isEmpty)

        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.contains(.reject(callPeer, "c1", false)) }

        #expect(f.store.state == busyState)
        #expect(!f.store.isFinished(sid: "c1"))
        #expect(tracker.managedCall?.sid == "other")
        f.store.clear()
    }
}
