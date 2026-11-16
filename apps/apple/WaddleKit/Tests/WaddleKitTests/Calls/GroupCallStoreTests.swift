import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

let groupRoom = BareJID(parsing: "room@muc.waddle.test")!
let groupMixer = JID(parsing: "calls.waddle.test/mixer")!
let groupJoin = CallJoin(
    url: "wss://livekit.waddle.test", room: "room@muc.waddle.test", identity: "alice@waddle.test/waddle-apple-1",
    token: callToken(#"{"sub":"alice@waddle.test/waddle-apple-1","video":{"roomJoin":true,"room":"room@muc.waddle.test"}}"#))

@MainActor @Suite("Group call store") struct GroupCallStoreTests {
    @Test func joinsInPresenceThenJingleOrder() async {
        let port = FakePort()
        let store = CallStore(port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare, newSid: { "g1" })
        let joining = Task { await store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { port.callSends.count == 1 }
        #expect(store.state == .joiningGroup(GroupCallAttempt(room: groupRoom, sid: "g1", media: callAudio, nick: "alice")))
        store.handleGroupPresence(WirePresence(
            from: groupRoom.with(resource: "alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: []),
            groupCall: GroupCallMarker(isPreparing: true, media: nil)))
        await eventually { port.callSends.count == 3 }
        store.handle(CallEvent(from: groupMixer, sid: "g1", kind: .sessionAccept(join: groupJoin, media: callAudio)))
        #expect(await joining.value)
        #expect(store.state == .inGroup(GroupCall(room: groupRoom, sid: "g1", media: callAudio, join: groupJoin, nick: "alice")))
        #expect(port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g1", false),
        ])
        store.clear()
    }
}

@MainActor final class GroupFixture {
    let port = FakePort()
    var own: JID? = callOwn
    let media: RecordingCallMedia?
    let timeout: Duration
    let acceptTimeout: Duration?
    let peersTimeout: Duration?
    let sid: () -> String
    init(media: RecordingCallMedia? = nil, timeout: Duration = .seconds(1),
         acceptTimeout: Duration? = nil, peersTimeout: Duration? = nil,
         sid: @escaping () -> String = { "g1" }) {
        self.media = media
        self.timeout = timeout
        self.acceptTimeout = acceptTimeout
        self.peersTimeout = peersTimeout
        self.sid = sid
    }
    lazy var store = CallStore(
        port: port, ownJID: { [weak self] in self?.own }, ownBareJID: callOwn.bare, media: media,
        groupPreparingEchoTimeout: timeout, groupOtherPreparingTimeout: peersTimeout ?? timeout, groupAcceptTimeout: acceptTimeout ?? timeout, newSid: sid)

    func begin(media: CallMedia = callAudio, room: BareJID = groupRoom) async -> Task<Bool, Never> {
        let task = Task { await store.joinGroupCall(in: room, media: media, nick: "alice") }
        await eventually { self.port.callSends.contains(.groupPresence(room, "alice", .init(state: .preparing))) }
        return task
    }
    func echo(room: BareJID = groupRoom) {
        store.handleGroupPresence(groupWire(nick: "alice", preparing: true, realJID: callOwn, room: room))
    }
    func toInitiate(media: CallMedia = callAudio) async -> Task<Bool, Never> {
        let task = await begin(media: media)
        echo()
        await eventually { self.port.callSends.contains(.groupInitiate(groupRoom, callOwn, "g1", media.video)) }
        return task
    }
    func accept(sid: String = "g1", from: JID = groupMixer, join: CallJoin = groupJoin) {
        store.handle(CallEvent(from: from, sid: sid, kind: .sessionAccept(join: join, media: callVideo)))
    }
    func activate(media: CallMedia = callAudio) async {
        let task = await toInitiate(media: media)
        accept()
        #expect(await task.value)
    }
}

func groupWire(nick: String, preparing: Bool = false, media: CallMedia? = nil, realJID: JID? = nil,
               room: BareJID = groupRoom) -> WirePresence {
    WirePresence(from: room.with(resource: nick)!, kind: .available,
        occupant: .init(affiliation: nil, role: nil, realJID: realJID, statusCodes: []),
        groupCall: .init(isPreparing: preparing, media: media))
}

extension GroupCallStoreTests {
    @Test func hangUpLeavesBeforeTerminatingAndRemovesOnlySelf() async {
        let f = GroupFixture()
        await f.activate()
        f.store.handleGroupPresence(groupWire(nick: "alice", media: callAudio, realJID: callOwn))
        f.store.handleGroupPresence(groupWire(nick: "bob", media: callVideo, realJID: callPeer))
        f.port.callSends = []
        await f.store.hangUp()
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        #expect(f.store.groupPresence.participants[groupRoom]?.map(\.nick) == ["bob"])
        f.store.clear()
    }
}

extension GroupCallStoreTests {
    @Test func joinWithoutMicAdvertisesMute() async {
        let f = GroupFixture()
        await f.activate(media: CallMedia(audio: false, video: false))
        #expect(f.store.isSelfMuteAdvertised)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isMuted: true)),
            .groupInitiate(groupRoom, callOwn, "g1", false)])
        f.store.clear()
    }
}

extension GroupCallStoreTests {
    @Test(arguments: ["sid", "room", "mixer"])
    func acceptMustMatchSidRoomAndMixer(mismatch: String) async {
        let f = GroupFixture()
        let joining = await f.toInitiate()
        let wrongRoom = CallJoin(url: groupJoin.url, room: "elsewhere@muc.waddle.test", identity: groupJoin.identity, token: groupJoin.token)
        switch mismatch {
        case "sid": f.accept(sid: "stale")
        case "room": f.accept(join: wrongRoom)
        default: f.accept(from: JID(parsing: "evil.waddle.test/mixer")!)
        }
        #expect(f.store.state == .joiningGroup(GroupCallAttempt(room: groupRoom, sid: "g1", media: callAudio, nick: "alice", isAdvertised: true)))
        f.accept()
        #expect(await joining.value)
        #expect(f.store.state == .inGroup(GroupCall(room: groupRoom, sid: "g1", media: callAudio, join: groupJoin, nick: "alice")))
        f.store.clear()
    }
}

extension GroupCallStoreTests {
    @Test func replayedAcceptDoesNotChangeGroup() async {
        let f = GroupFixture()
        await f.activate()
        f.accept(join: callJoin)
        #expect(f.store.state == .inGroup(GroupCall(room: groupRoom, sid: "g1", media: callAudio, join: groupJoin, nick: "alice")))
        #expect(f.port.callSends.count == 3)
        f.store.clear()
    }
}

extension GroupCallStoreTests {
    @Test func echoTimeoutLeavesWithoutTerminate() async {
        let f = GroupFixture(timeout: .milliseconds(30))
        let joining = await f.begin()
        #expect(await !joining.value)
        #expect(f.store.state == .idle)
        #expect(f.store.lastError == "muc call preparation failed")
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .preparing)), .groupPresence(groupRoom, "alice", .init(state: .left))])
    }
}

extension GroupCallStoreTests {
    @Test func failedInitiateLeavesAndTerminates() async {
        let f = GroupFixture()
        f.port.callResults["groupInitiate"] = false
        let joining = await f.toInitiate()
        #expect(await !joining.value)
        #expect(f.store.state == .idle)
        #expect(f.store.lastError == "muji session initiate failed")
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g1", false),
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
    }
}

extension GroupCallStoreTests {
    @Test func acceptTimeoutLeavesAndTerminates() async {
        let f = GroupFixture(acceptTimeout: .milliseconds(80))
        let joining = await f.toInitiate()
        #expect(await !joining.value)
        #expect(f.store.state == .idle)
        #expect(f.store.lastError == "muji session initiate failed")
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g1", false),
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
    }
}

extension GroupCallStoreTests {
    @Test func hangUpDuringPreparationCancelsWaitersWithoutTerminate() async {
        let f = GroupFixture()
        let joining = await f.begin()
        await f.store.hangUp()
        #expect(await !joining.value)
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .left)),
            .groupPresence(groupRoom, "alice", .init(state: .left))])
        #expect(f.store.lastError == nil)
    }
}

extension GroupCallStoreTests {
    @Test func heldPreparingSendCannotOvertakeLeave() async {
        let f = GroupFixture()
        f.port.heldCallVerbs = ["groupPresence"]
        let joining = await f.begin()
        let hangingUp = Task { await f.store.hangUp() }
        await eventually { f.store.state == .idle }
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .preparing))])
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await hangingUp.value
        #expect(await !joining.value)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .left)),
            .groupPresence(groupRoom, "alice", .init(state: .left))])
    }
}

extension GroupCallStoreTests {
    @Test func acceptResolvedBeforeTeardownTerminatesAgain() async {
        let f = GroupFixture()
        let joining = await f.toInitiate()
        f.accept()
        await f.store.hangUp()
        #expect(await !joining.value)
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g1", false),
            .groupPresence(groupRoom, "alice", .init(state: .left)),
            .groupTerminate(groupRoom, "g1"), .groupTerminate(groupRoom, "g1")])
    }
}

extension GroupCallStoreTests {
    @Test(arguments: [false, true], ["terminate", "finish", "reject", "retract"])
    func mixerEndsBothGroupPhases(active: Bool, verb: String) async {
        let f = GroupFixture()
        var joining: Task<Bool, Never>?
        if active { await f.activate() } else { joining = await f.begin() }
        let kind: CallEvent.Kind
        let reason: CallEndReason
        switch verb {
        case "terminate": kind = .sessionTerminate(reason: .success); reason = .finished(.success)
        case "finish": kind = .finish(reason: .success, migratedTo: nil); reason = .finished(.success)
        case "reject": kind = .reject(reason: .expired, tieBreak: true); reason = .expired
        default: kind = .retract(reason: nil, tieBreak: false); reason = .retracted
        }
        f.port.callSends = []
        f.store.handle(CallEvent(from: groupMixer, sid: "g1", kind: kind))
        await eventually { !f.store.state.isLive }
        if let joining { #expect(await !joining.value) }
        await eventually { f.port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .left))) }
        #expect(f.store.state == .ended(sid: "g1", reason: reason))
        if active {
            await eventually { f.port.callSends.last == .groupTerminate(groupRoom, "g1") }
            #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        } else {
            #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .left)), .groupPresence(groupRoom, "alice", .init(state: .left))])
        }
        f.store.clear()
    }
}
