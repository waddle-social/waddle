import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Group call races") struct GroupCallRaceTests {
    @Test func callerCancellationDuringActivePresenceRollsBack() async {
        let f = GroupFixture()
        let joining = await f.begin()
        f.port.heldCallVerbs = ["groupPresence"]
        f.echo()
        await eventually { f.port.callSends.count == 2 }
        joining.cancel()
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await !joining.value)
        #expect(f.store.state == .ended(sid: "g1", reason: .error))
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        f.store.clear()
    }
}

extension GroupCallRaceTests {
    @Test func heldActiveSendCannotInitiateAfterHangUp() async {
        let f = GroupFixture()
        let joining = await f.begin()
        f.port.heldCallVerbs = ["groupPresence"]
        f.echo()
        await eventually { f.port.callSends.count == 2 }
        let leave = Task { await f.store.hangUp() }
        await eventually { f.store.state == .idle }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await !joining.value)
        await leave.value
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupPresence(groupRoom, "alice", .init(state: .left)),
            .groupPresence(groupRoom, "alice", .init(state: .left))])
        #expect(f.store.lastError == nil)
        f.store.clear()
    }
}

extension GroupCallRaceTests {
    @Test func boundResourceLookupCannotReopenAnAbandonedAttempt() async {
        let port = FakePort()
        var store: CallStore?
        var lookups = 0
        store = CallStore(port: port, ownJID: {
            lookups += 1
            if lookups == 2 { store?.clear() }
            return callOwn
        }, ownBareJID: callOwn.bare, newSid: { "g1" })
        guard let calls = store else { return }
        let joining = Task { await calls.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { port.callSends.count == 1 }
        calls.handleGroupPresence(groupWire(nick: "alice", preparing: true, realJID: callOwn))
        #expect(await !joining.value)
        #expect(calls.state == .idle)
        #expect(port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false)))])
        #expect(calls.lastError == nil)
        store = nil
    }
}

extension GroupCallRaceTests {
    @Test func abandonedClearCannotWipeNewSameRoomAttempt() async {
        var counter = 0
        let f = GroupFixture(sid: { counter += 1; return "g\(counter)" })
        f.port.heldCallVerbs = ["groupPresence"]
        let first = await f.begin()
        let leave = Task { await f.store.hangUp() }
        await eventually { f.store.state == .idle }
        let second = Task { await f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { f.store.state.sid == "g2" }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await eventually { f.port.callSends.count == 2 }
        f.echo()
        await eventually { f.port.callSends.contains(.groupInitiate(groupRoom, callOwn, "g2", false)) }
        f.accept(sid: "g2")
        #expect(await !first.value)
        #expect(await second.value)
        await leave.value
        #expect(f.store.state == .inGroup(GroupCall(room: groupRoom, sid: "g2", media: callAudio, join: groupJoin, nick: "alice")))
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g2", false)])
        f.store.clear()
    }
}

extension GroupCallRaceTests {
    @Test(arguments: [false, true], ["terminate", "finish", "reject", "retract"])
    func onlyMixerAndMatchingSidCanEndGroup(active: Bool, verb: String) async {
        let f = GroupFixture()
        let joining: Task<Bool, Never>?
        if active { await f.activate(); joining = nil } else { joining = await f.toInitiate() }
        let kind: CallEvent.Kind
        switch verb {
        case "terminate": kind = .sessionTerminate(reason: .success)
        case "finish": kind = .finish(reason: .success, migratedTo: nil)
        case "reject": kind = .reject(reason: .expired, tieBreak: true)
        default: kind = .retract(reason: nil, tieBreak: false)
        }
        f.port.callSends = []
        f.store.handle(CallEvent(from: groupMixer, sid: "stale", kind: kind))
        f.store.handle(CallEvent(from: callPeer, sid: "g1", kind: kind))
        f.store.handle(CallEvent(from: JID(parsing: "alice@waddle.test/tablet")!, sid: "g1", kind: kind))
        f.store.handle(CallEvent(from: callPeer, sid: "barrier", kind: .propose(callAudio)))
        await eventually { f.port.callSends.last == .reject(callPeer, "barrier", false) }
        #expect(f.port.callSends == [.reject(callPeer, "barrier", false)])
        if active {
            #expect(f.store.state == .inGroup(GroupCall(room: groupRoom, sid: "g1", media: callAudio, join: groupJoin, nick: "alice")))
        } else {
            #expect(f.store.state == .joiningGroup(GroupCallAttempt(room: groupRoom, sid: "g1", media: callAudio, nick: "alice", isAdvertised: true)))
        }
        f.store.clear()
        if let joining { #expect(await !joining.value) }
    }
}

extension GroupCallRaceTests {
    @Test(arguments: [false, true])
    func groupSlotBusyRejectsProposalsAndRefusesLocalStarts(active: Bool) async {
        let f = GroupFixture()
        let joining: Task<Bool, Never>?
        if active { await f.activate(); joining = nil } else { joining = await f.begin() }
        f.port.callSends = []
        #expect(await !f.store.startCall(to: callPeer.bare, media: callAudio))
        #expect(await !f.store.joinGroupCall(in: groupRoom, media: callVideo, nick: "alice"))
        f.store.handle(CallEvent(from: callPeer, sid: "dm", kind: .propose(callVideo)))
        await eventually { f.port.callSends == [.reject(callPeer, "dm", false)] }
        #expect(f.port.callSends == [.reject(callPeer, "dm", false)])
        #expect(f.store.state.isGroupPhase)
        f.store.clear()
        if let joining { #expect(await !joining.value) }
    }
}

extension GroupCallRaceTests {
    @Test func directCallRefusesGroupJoin() async {
        let f = GroupFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        #expect(await !f.store.joinGroupCall(in: groupRoom, media: callVideo, nick: "alice"))
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "g1", media: callAudio, initiator: callOwn)))
        #expect(f.port.callSends == [.propose(callPeer.bare, "g1", callAudio)])
        f.store.clear()
    }
}

extension GroupCallRaceTests {
    @Test func failedLeaveStillTerminatesAndReportsError() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.callResults["groupPresence"] = false
        await f.store.hangUp()
        #expect(f.store.state == .idle)
        #expect(f.store.lastError == "muji leave presence failed")
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        f.store.clear()
    }
}

extension GroupCallRaceTests {
    @Test func otherPreparingMustFinishBeforeContents() async {
        let f = GroupFixture()
        f.store.handleGroupPresence(groupWire(nick: "bob", preparing: true, realJID: callPeer))
        let joining = await f.begin()
        f.echo()
        f.store.handle(CallEvent(from: callPeer, sid: "barrier", kind: .propose(callAudio)))
        await eventually { f.port.callSends.last == .reject(callPeer, "barrier", false) }
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .preparing)), .reject(callPeer, "barrier", false)])
        f.store.handleGroupPresence(groupWire(nick: "bob", media: callVideo, realJID: callPeer))
        await eventually { f.port.callSends.last == .groupInitiate(groupRoom, callOwn, "g1", false) }
        f.accept()
        #expect(await joining.value)
        #expect(f.store.groupPresence.participants[groupRoom]?.map(\.nick) == ["bob"])
        f.store.clear()
    }
}

extension GroupCallRaceTests {
    @Test func otherPreparingTimeoutRollsBack() async {
        let f = GroupFixture(peersTimeout: .milliseconds(50))
        f.store.handleGroupPresence(groupWire(nick: "bob", preparing: true, realJID: callPeer))
        let joining = await f.begin()
        f.echo()
        #expect(await !joining.value)
        #expect(f.store.state == .ended(sid: "g1", reason: .error))
        #expect(f.store.lastError == "muc call preparation failed")
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .preparing)), .groupPresence(groupRoom, "alice", .init(state: .left))])
    }
}

extension GroupCallRaceTests {
    @Test func missingBoundResourceRollsBackAdvertisedAttempt() async {
        let f = GroupFixture()
        let joining = await f.begin()
        f.own = nil
        f.echo()
        #expect(await !joining.value)
        #expect(f.store.state == .ended(sid: "g1", reason: .error))
        #expect(f.store.lastError == "muji session initiate failed: no bound resource")
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
    }
}
