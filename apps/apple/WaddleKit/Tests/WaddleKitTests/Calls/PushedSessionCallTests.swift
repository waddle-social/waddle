import Foundation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Pushed session calls") struct PushedSessionCallTests {
    private func session(handlesCalls: Bool = true) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource),
            port: port, handlesCalls: handlesCalls)
        return (coordinator, port)
    }

    private func push(sid: String = "c-push", from: String? = callPeer.description, expired: Bool = false) -> CallPush {
        var call: [String: Any] = ["sid": sid, "video": true, "expires": expired ? 1 : 4_102_444_800]
        call["from"] = from
        return CallPush(userInfo: ["waddle": [
            "v": 1, "class": "call", "conversation": callPeer.bare.description,
            "item": "stanza", "node": "voip-node", "call": call,
        ]])!
    }

    private func drain(_ store: CallStore) async {
        var drained = false
        store.enqueue { drained = true }
        await eventually { drained }
    }

    @Test func offlineFailureIsReplayedOnceOnlineAndStreamCopyHasNoEffects() async {
        let (c, port) = session()
        port.callResults["ringing"] = false
        #expect(c.receivePushedCall(push()))
        await drain(c.calls)
        #expect(c.calls.state == .incoming(IncomingCall(from: callPeer, sid: "c-push", media: callVideo)))
        #expect(port.callSends == [.ringing(callPeer.bare, "c-push")])
        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        port.callResults["ringing"] = true
        c.handle(.connected)
        await drain(c.calls)
        c.handle(.connected)
        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        await drain(c.calls)
        #expect(port.callSends == [.ringing(callPeer.bare, "c-push"), .ringing(callPeer.bare, "c-push")])
        await c.stop()
    }

    @Test(arguments: [false, true])
    func onlineBeforeFirstSendSettlesDoesNotRaceTheRetry(firstSendSucceeds: Bool) async {
        let (c, port) = session()
        port.callResults["ringing"] = firstSendSucceeds
        port.heldCallVerbs = ["ringing"]
        #expect(c.receivePushedCall(push()))
        await eventually { port.callSends.count == 1 }
        c.handle(.connected)
        c.handle(.connected)
        #expect(port.callSends.count == 1)
        port.callResults["ringing"] = true
        port.heldCallVerbs = []
        port.releaseCallSends()
        await eventually { port.completedCallSends.count == (firstSendSucceeds ? 1 : 2) }
        await drain(c.calls)
        #expect(port.callSends.count == (firstSendSucceeds ? 1 : 2))
        await c.stop()
    }

    @Test func successfulOfflineAttemptIsNotReplayed() async {
        let (c, port) = session()
        #expect(c.receivePushedCall(push()))
        await drain(c.calls)
        c.handle(.connected)
        await drain(c.calls)
        #expect(port.callSends == [.ringing(callPeer.bare, "c-push")])
        await c.stop()
    }

    @Test func failedRetryIsNotRepeatedOnAnotherOnlineTransition() async {
        let (c, port) = session()
        port.callResults["ringing"] = false
        #expect(c.receivePushedCall(push()))
        await drain(c.calls)
        c.handle(.connected)
        await drain(c.calls)
        c.handle(.disconnected)
        c.handle(.connected)
        await drain(c.calls)
        #expect(port.callSends.count == 2)
        await c.stop()
    }

    @Test(arguments: ["decline", "accept", "finish", "clear"])
    func aCallNoLongerRingingIsNotReplayed(action: String) async {
        let (c, port) = session()
        port.callResults["ringing"] = false
        #expect(c.receivePushedCall(push()))
        await drain(c.calls)
        switch action {
        case "decline": #expect(await c.calls.declineIncoming())
        case "accept": #expect(await c.calls.acceptIncoming())
        case "finish": c.calls.markFinished(sid: "c-push")
        default: c.calls.clear()
        }
        c.handle(.connected)
        await drain(c.calls)
        #expect(port.callSends.filter { $0 == .ringing(callPeer.bare, "c-push") }.count == 1)
        await c.stop()
    }

    @Test func onlinePushAndStreamCopyProduceOneSetOfEffects() async {
        let (c, port) = session()
        c.handle(.connected)
        #expect(c.receivePushedCall(push()))
        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        #expect(c.receivePushedCall(push()))
        await drain(c.calls)
        #expect(port.callSends == [.ringing(callPeer.bare, "c-push")])
        await c.stop()
    }

    @Test func streamCopyDoesNotRepeatABusyResponse() async {
        let (c, port) = session()
        let other = JID(parsing: "carol@waddle.test/tablet")!
        c.handle(.call(CallEvent(from: other, sid: "other", kind: .propose(callAudio))))
        await drain(c.calls)
        #expect(!c.receivePushedCall(push()))
        await drain(c.calls)
        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        await drain(c.calls)
        #expect(port.callSends == [.ringing(other.bare, "other"), .reject(callPeer, "c-push", false)])
        await c.stop()
    }

    @Test func streamCopyDoesNotRepeatASuspendedTieBreak() async {
        let (c, port) = session()
        #expect(await c.calls.startCall(to: callPeer.bare, media: callAudio))
        port.heldCallVerbs = ["retractTieBreak"]
        #expect(!c.receivePushedCall(push(sid: "a-winner")))
        await eventually { port.callSends.count == 2 }
        c.handle(.call(CallEvent(from: callPeer, sid: "a-winner", kind: .propose(callVideo))))
        port.heldCallVerbs = []
        port.releaseCallSends()
        await drain(c.calls)
        #expect(port.callSends.count == 2)
        #expect(c.calls.state == .incoming(IncomingCall(from: callPeer, sid: "a-winner", media: callVideo)))
        await c.stop()
    }

    @Test func streamCopyDoesNotRepeatASuspendedMigration() async {
        let (c, port) = session()
        #expect(await c.calls.startCall(to: callPeer.bare, media: callAudio))
        let oldSid = c.calls.state.sid!
        c.handle(.call(CallEvent(from: callPeer, sid: oldSid, kind: .sessionAccept(join: callJoin, media: callAudio))))
        await drain(c.calls)
        port.callSends = []
        port.heldCallVerbs = ["migrated"]
        #expect(!c.receivePushedCall(push()))
        await eventually { port.callSends == [.migrated(callPeer, oldSid, "c-push")] }
        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        port.heldCallVerbs = []
        port.releaseCallSends()
        await eventually { port.callSends.count >= 3 }
        await drain(c.calls)
        #expect(port.callSends.filter { if case .migrated = $0 { true } else { false } }.count == 1)
        #expect(!port.callSends.contains(.reject(callPeer, "c-push", false)))
        #expect(c.calls.state == .incoming(IncomingCall(from: callPeer, sid: "c-push", media: callAudio, isAccepting: true)))
        await c.stop()
    }

    @Test func invalidOrFinishedPushesNeverReachTheSlot() async {
        let (c, port) = session()
        #expect(!c.receivePushedCall(push(expired: true)))
        #expect(!c.receivePushedCall(push(from: nil)))
        c.calls.markFinished(sid: "c-push")
        #expect(!c.receivePushedCall(push()))
        await drain(c.calls)
        #expect(c.calls.state == .idle)
        #expect(port.callSends.isEmpty)
        await c.stop()
    }

    @Test func pushesShareTheStreamPolicyAndStopGuard() async {
        let (disabled, disabledPort) = session(handlesCalls: false)
        #expect(!disabled.receivePushedCall(push()))
        disabled.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        #expect(disabled.calls.state == .idle)
        #expect(disabledPort.callSends.isEmpty)
        await disabled.stop()
        let (stopped, port) = session()
        await stopped.stop()
        #expect(!stopped.receivePushedCall(push()))
        stopped.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        #expect(stopped.calls.state == .idle)
        #expect(port.callSends.isEmpty)
    }
}
