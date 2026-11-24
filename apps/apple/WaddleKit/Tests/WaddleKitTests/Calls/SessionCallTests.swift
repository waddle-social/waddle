import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Session calls")
struct SessionCallTests {
    @Test func injectedMediaFollowsTheSessionCallStore() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource),
            port: port, handlesCalls: true, callMedia: media)
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c-media", kind: .propose(callAudio))))
        await eventually { coordinator.calls.state != .idle }
        #expect(await coordinator.calls.acceptIncoming())
        port.emit(.call(CallEvent(from: callPeer, sid: "c-media", kind: .sessionInitiate(join: callJoin, media: callAudio))))
        await eventually { media.calls == [.connect(callJoin, callAudio)] }
        #expect(media.calls == [.connect(callJoin, callAudio)])
        #expect(port.externalServicesFetchCount == 1)
        await coordinator.stop()
        await eventually { media.calls.last == .disconnect }
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect])
    }

    @Test func streamLossPreservesTheActiveCallSlot() async {
        let (coordinator, port) = session()
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio))))
        await eventually { coordinator.calls.state != .idle }
        #expect(await coordinator.calls.acceptIncoming())
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .sessionInitiate(join: callJoin, media: callAudio))))
        await eventually { port.callSends.contains(.accept(callPeer, callOwn, "c1", callAudio)) }
        port.callSends = []
        port.emit(.disconnected)
        // The next arrival proves stream loss was consumed first.
        port.emit(.authenticationFailed)
        await eventually { coordinator.connection == .authenticationFailed }
        #expect(
            coordinator.calls.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callPeer))
        )
        #expect(port.callSends.isEmpty)
        await coordinator.stop()
    }

    @Test func stopRetractsOutgoingCall() async {
        let (coordinator, port) = session()
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        guard case .outgoing(let call) = coordinator.calls.state else {
            Issue.record("expected outgoing call")
            return
        }
        port.callSends = []
        await coordinator.stop()
        #expect(port.callSends == [.retract(callPeer.bare, call.sid)])
        #expect(port.disconnectCount == 1)
        #expect(coordinator.calls.state == .idle)
    }

    @Test func stopBoundsHeldTerminateAndIgnoresNewInvitations() async {
        let (coordinator, port) = session()
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio))))
        await eventually { coordinator.calls.state != .idle }
        #expect(await coordinator.calls.acceptIncoming())
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .sessionInitiate(join: callJoin, media: callAudio))))
        await eventually { port.callSends.contains(.accept(callPeer, callOwn, "c1", callAudio)) }
        port.callSends = []
        port.completedCallSends = []
        port.heldCallVerbs = ["terminate"]
        var stopped = false
        let stopping = Task {
            await coordinator.stop()
            stopped = true
        }
        await eventually { port.callSends == [.terminate(callPeer, "c1", .success)] }
        #expect(port.disconnectCount == 0)
        port.emit(.call(CallEvent(from: callPeer, sid: "c2", kind: .propose(callAudio))))
        port.emit(.authenticationFailed)
        await eventually { coordinator.connection == .authenticationFailed }
        #expect(coordinator.calls.state == .idle)
        #expect(port.callSends == [.terminate(callPeer, "c1", .success)])
        await eventually(timeout: 4) { stopped }
        #expect(stopped)
        #expect(port.disconnectCount == 1)
        #expect(coordinator.calls.state == .idle)
        port.heldCallVerbs = []
        port.releaseCallSends()
        await eventually { port.completedCallSends.contains(.terminate(callPeer, "c1", .success)) }
        #expect(port.callSends == [.terminate(callPeer, "c1", .success)])
        #expect(coordinator.calls.lastError == nil)
        await stopping.value
    }

    @Test(arguments: [false, true]) func stopAnswersIncomingCallBeforeDisconnecting(accepting: Bool) async {
        let (coordinator, port) = session()
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio))))
        await eventually { port.callSends == [.ringing(callPeer, "c1")] }
        if accepting { #expect(await coordinator.calls.acceptIncoming()) }
        port.callSends = []
        await coordinator.stop()
        if accepting {
            #expect(port.callSends == [.finish(callPeer, "c1", .cancel)])
        } else {
            #expect(port.callSends == [.reject(callPeer, "c1", false)])
        }
        #expect(port.disconnectCount == 1)
        #expect(coordinator.calls.state == .idle)
    }

    @Test func stopTerminatesActiveCallAndSendsFinishBeforeDisconnecting() async {
        let (coordinator, port) = session()
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio))))
        await eventually { coordinator.calls.state != .idle }
        #expect(await coordinator.calls.acceptIncoming())
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .sessionInitiate(join: callJoin, media: callAudio))))
        await eventually { port.callSends.contains(.accept(callPeer, callOwn, "c1", callAudio)) }
        port.callSends = []
        await coordinator.stop()
        #expect(port.callSends == [.terminate(callPeer, "c1", .success), .finish(callPeer, "c1", nil)])
        #expect(port.disconnectCount == 1)
        #expect(coordinator.calls.state == .idle)
    }

    private func session() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource), port: port, handlesCalls: true)
        return (coordinator, port)
    }

    @Test func optedInSessionRoutesCallEventsInStreamOrder() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource), port: port, handlesCalls: true)
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio))))
        await eventually { port.callSends == [.ringing(callPeer, "c1")] }
        #expect(coordinator.calls.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(port.callSends == [.ringing(callPeer, "c1")])
        await coordinator.stop()
    }

    @Test func callsAreIgnoredByDefaultUntilTheUIShips() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource), port: port)
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio))))
        // A later event is an arrival-order barrier for the ignored call.
        port.emit(.authenticationFailed)
        await eventually { coordinator.connection == .authenticationFailed }
        #expect(coordinator.calls.state == .idle)
        #expect(port.callSends.isEmpty)
        await coordinator.stop()
    }

    @Test(arguments: [nil, "", "waddle-apple-1"] as [String?])
    func sessionHandshakeUsesOnlyKnownFullResource(resource: String?) async {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: resource), port: port, handlesCalls: true)
        coordinator.start()
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio))))
        await eventually { port.callSends.count == 1 }
        #expect(await coordinator.calls.acceptIncoming())
        port.emit(.call(CallEvent(from: callPeer, sid: "c1", kind: .sessionInitiate(join: callJoin, media: callAudio))))
        if resource == "waddle-apple-1" {
            await eventually { port.callSends.count == 3 }
            #expect(
                port.callSends == [
                    .ringing(callPeer, "c1"), .proceed(callPeer, "c1"), .accept(callPeer, callOwn, "c1", callAudio),
                ])
        } else {
            await eventually { coordinator.calls.lastError != nil }
            #expect(coordinator.calls.state == .ended(sid: "c1", reason: .error))
            #expect(coordinator.calls.lastError == "call session accept failed: no bound resource")
            #expect(port.callSends == [.ringing(callPeer, "c1"), .proceed(callPeer, "c1")])
        }
        await coordinator.stop()
    }
}
