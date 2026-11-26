import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Call termination") struct CallTerminationTests {
    @Test func quittingWaitsForTheStreamToCloseAfterHangUp() async throws {
        let (coordinator, port) = await connectedSession(emitsDisconnectEvent: false)
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        let sid = try #require(coordinator.calls.state.sid)
        port.heldCallVerbs = ["retract"]

        var completions = 0
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { completions += 1 })
        await eventually { port.callSends.last == .retract(callPeer.bare, sid) }
        #expect(port.disconnectCount == 0)
        port.releaseCallSends()

        await eventually { port.disconnectCount == 1 || completions > 0 }
        #expect(port.disconnectCount == 1)
        #expect(completions == 0)
        #expect(port.completedCallSends.last == .retract(callPeer.bare, sid))

        port.emit(.disconnected)
        await eventually { completions > 0 }
        #expect(completions == 1)
        #expect(coordinator.connection == .offline(retryAt: nil))
        coordinator.resume()
        coordinator.start()
        #expect(coordinator.connection == .offline(retryAt: nil))
        #expect(port.connectCount == 1)
    }

    @Test func quittingStopsWaitingAtTheBoundWhenTheStreamNeverCloses() async throws {
        let (coordinator, port) = await connectedSession(emitsDisconnectEvent: false)
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        let sid = try #require(coordinator.calls.state.sid)

        let timeout: Duration = .seconds(2)
        let requestedAt = ContinuousClock.now
        var completions: [ContinuousClock.Instant] = []
        #expect(coordinator.beginCallTermination(timeout: timeout) { completions.append(.now) })
        await eventually { !completions.isEmpty }

        #expect(port.disconnectCount == 1)
        #expect(port.completedCallSends.last == .retract(callPeer.bare, sid))
        #expect(completions.count == 1)
        let completion = try #require(completions.first)
        #expect(completion >= requestedAt + timeout)
        port.emit(.disconnected)
        await eventually { coordinator.connection == .offline(retryAt: nil) }
        #expect(completions.count == 1)
    }

    @Test func quittingFinishesAnAcceptingCallThatReplacedOurs() async throws {
        let (coordinator, port) = await connectedSession()
        let calls = coordinator.calls
        #expect(await calls.startCall(to: callPeer.bare, media: callAudio))

        var completed = false
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { completed = true })
        // Install the winner without yielding so the quit task sees the new slot.
        let replacement = CallEvent(from: callPeer, sid: "replacement", kind: .propose(callAudio))
        try #require(calls.installIncoming(replacement, media: callAudio, expected: calls.state, accepting: true, epoch: calls.generation))
        await eventually { completed }

        #expect(completed)
        #expect(calls.state == .idle)
        #expect(port.completedCallSends.contains(.finish(callPeer, "replacement", .cancel)))
    }

    @Test func quittingCompletesAfterHangUpIfTheStreamAlreadyClosed() async throws {
        let (coordinator, port) = await connectedSession(emitsDisconnectEvent: false)
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        let sid = try #require(coordinator.calls.state.sid)
        port.heldCallVerbs = ["retract"]

        var completions = 0
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { completions += 1 })
        await eventually { port.callSends.last == .retract(callPeer.bare, sid) }
        port.emit(.disconnected)
        await eventually { coordinator.connection == .offline(retryAt: nil) }
        #expect(completions == 0)
        port.releaseCallSends()

        await eventually { completions > 0 }
        #expect(completions == 1)
        #expect(port.completedCallSends.last == .retract(callPeer.bare, sid))
        #expect(port.disconnectCount == 0)
    }

    @Test func quittingEndsAnActiveCall() async throws {
        let (coordinator, port) = await connectedSession()
        let calls = coordinator.calls
        #expect(await calls.startCall(to: callPeer.bare, media: callAudio))
        let sid = try #require(calls.state.sid)
        calls.handle(.init(from: callPeer, sid: sid, kind: .proceed))
        calls.handle(.init(from: callPeer, sid: sid, kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { port.completedCallSends.count == 2 }
        port.completedCallSends = []

        var sendsAtCompletion: [CallSend]?
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { sendsAtCompletion = port.completedCallSends })
        await eventually { sendsAtCompletion != nil }

        #expect(calls.state == .idle)
        #expect(sendsAtCompletion == [.terminate(callPeer, sid, .success), .finish(callPeer, sid, nil)])
    }

    @Test func quittingLeavesTheRoomCall() async throws {
        let (coordinator, port) = await connectedSession()
        let calls = coordinator.calls
        let joining = Task { await calls.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        let sid = try #require(calls.state.sid)
        calls.handleGroupPresence(groupWire(nick: "alice", preparing: true, realJID: callOwn))
        await eventually { port.callSends.contains(.groupInitiate(groupRoom, callOwn, sid, false)) }
        calls.handle(.init(from: groupMixer, sid: sid, kind: .sessionAccept(join: groupJoin, media: callAudio)))
        #expect(await joining.value)
        port.completedCallSends = []

        var sendsAtCompletion: [CallSend]?
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { sendsAtCompletion = port.completedCallSends })
        await eventually { sendsAtCompletion != nil }

        #expect(calls.state == .idle)
        #expect(sendsAtCompletion == [
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, sid),
        ])
    }

    @Test func quittingWithoutACallDoesNotWait() async {
        let (coordinator, port) = await connectedSession()

        #expect(!coordinator.beginCallTermination(timeout: .seconds(30)) { Issue.record("No hang-up was started") })
        #expect(coordinator.calls.state == .idle)
        #expect(port.disconnectCount == 0)
        #expect(port.callSends.isEmpty)
    }

    @Test func quittingLeavesAnUnansweredIncomingCallAlone() async {
        let (coordinator, port) = await connectedSession()
        let calls = coordinator.calls
        calls.handle(.init(from: callPeer, sid: "c1", kind: .propose(callAudio)))
        await eventually { port.completedCallSends.count == 1 }

        #expect(!coordinator.beginCallTermination(timeout: .seconds(30)) { Issue.record("The call was never answered here") })
        #expect(calls.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(port.disconnectCount == 0)
        #expect(port.callSends == [.ringing(callPeer, "c1")])
        calls.clear()
    }

    @Test func quittingRetractsAnOutgoingRing() async throws {
        let (coordinator, port) = await connectedSession()
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        let sid = try #require(coordinator.calls.state.sid)

        var sendsAtCompletion: [CallSend]?
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { sendsAtCompletion = port.completedCallSends })
        await eventually { sendsAtCompletion != nil }

        #expect(coordinator.calls.state == .idle)
        #expect(sendsAtCompletion == [.propose(callPeer.bare, sid, callAudio), .retract(callPeer.bare, sid)])
    }

    @Test func quittingFinishesACallWeAnsweredThatIsStillConnecting() async {
        let (coordinator, port) = await connectedSession()
        let calls = coordinator.calls
        calls.handle(.init(from: callPeer, sid: "c1", kind: .propose(callAudio)))
        await eventually { port.completedCallSends.count == 1 }
        #expect(await calls.acceptIncoming())

        var sendsAtCompletion: [CallSend]?
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { sendsAtCompletion = port.completedCallSends })
        await eventually { sendsAtCompletion != nil }

        #expect(calls.state == .idle)
        #expect(sendsAtCompletion == [.ringing(callPeer, "c1"), .proceed(callPeer, "c1"), .finish(callPeer, "c1", .cancel)])
    }

    @Test func quittingDoesNotHangUpAReplacementRing() async throws {
        let (coordinator, port) = await connectedSession()
        let calls = coordinator.calls
        #expect(await calls.startCall(to: callPeer.bare, media: callAudio))
        let sid = try #require(calls.state.sid)

        var completed = false
        #expect(coordinator.beginCallTermination(timeout: .seconds(30)) { completed = true })
        calls.clear()
        calls.handle(.init(from: callPeer, sid: "c2", kind: .propose(callAudio)))
        await eventually { completed && port.completedCallSends.count >= 2 }

        #expect(completed)
        #expect(calls.state == .incoming(IncomingCall(from: callPeer, sid: "c2", media: callAudio)))
        #expect(port.callSends == [.propose(callPeer.bare, sid, callAudio), .ringing(callPeer, "c2")])
        calls.clear()
    }

    @Test func quittingStopsWaitingEvenWhenTheSendDoesNotComplete() async throws {
        let (coordinator, port) = await connectedSession()
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        let sid = try #require(coordinator.calls.state.sid)
        port.heldCallVerbs = ["retract"]

        let timeout: Duration = .seconds(2)
        let requestedAt = ContinuousClock.now
        var completions: [ContinuousClock.Instant] = []
        #expect(coordinator.beginCallTermination(timeout: timeout) { completions.append(.now) })
        await eventually { !completions.isEmpty }

        #expect(completions.count == 1)
        let completion = try #require(completions.first)
        #expect(completion >= requestedAt + timeout)
        #expect(port.callSends.last == .retract(callPeer.bare, sid))
        #expect(port.completedCallSends == [.propose(callPeer.bare, sid, callAudio)])
        #expect(port.disconnectCount == 0)

        port.releaseCallSends()
        await eventually { port.completedCallSends.last == .retract(callPeer.bare, sid) }
        #expect(completions.count == 1)
    }

    private func connectedSession(emitsDisconnectEvent: Bool = true) async -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        port.emitsDisconnectEvent = emitsDisconnectEvent
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource),
            port: port, handlesCalls: true)
        coordinator.start()
        await eventually { port.connectCount == 1 }
        port.emit(.connected)
        await eventually { coordinator.connection == .online }
        return (coordinator, port)
    }
}
