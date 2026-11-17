import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Group clear generation") struct GroupCallGenerationTests {
    @Test(arguments: ["preparing", "active", "initiate", "leave", "terminate"])
    func clearFencesEverySuspendedGroupVerb(stage: String) async {
        let f = GroupFixture()
        var joining: Task<Bool, Never>?
        var leaving: Task<Void, Never>?
        let expected: [CallSend]
        switch stage {
        case "preparing":
            f.port.heldCallVerbs = ["groupPresence"]
            f.port.callResults["groupPresence"] = false
            joining = await f.begin()
            expected = [.groupPresence(groupRoom, "alice", .init(state: .preparing))]
        case "active":
            joining = await f.begin()
            f.port.heldCallVerbs = ["groupPresence"]
            f.port.callResults["groupPresence"] = false
            f.echo()
            await eventually { f.port.callSends.count == 2 }
            expected = [
                .groupPresence(groupRoom, "alice", .init(state: .preparing)),
                .groupPresence(groupRoom, "alice", .init(state: .active(video: false)))]
        case "initiate":
            f.port.heldCallVerbs = ["groupInitiate"]
            f.port.callResults["groupInitiate"] = false
            joining = await f.toInitiate()
            expected = [
                .groupPresence(groupRoom, "alice", .init(state: .preparing)),
                .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
                .groupInitiate(groupRoom, callOwn, "g1", false)]
        case "leave":
            await f.activate()
            f.port.heldCallVerbs = ["groupPresence"]
            f.port.callResults["groupPresence"] = false
            leaving = Task { await f.store.hangUp() }
            await eventually { f.port.callSends.count == 4 }
            expected = [
                .groupPresence(groupRoom, "alice", .init(state: .preparing)),
                .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
                .groupInitiate(groupRoom, callOwn, "g1", false),
                .groupPresence(groupRoom, "alice", .init(state: .left))]
        default:
            await f.activate()
            f.port.heldCallVerbs = ["groupTerminate"]
            f.port.callResults["groupTerminate"] = false
            leaving = Task { await f.store.hangUp() }
            await eventually { f.port.callSends.count == 5 }
            expected = [
                .groupPresence(groupRoom, "alice", .init(state: .preparing)),
                .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
                .groupInitiate(groupRoom, callOwn, "g1", false),
                .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")]
        }
        #expect(f.port.callSends == expected)
        f.store.clear()
        f.port.heldCallVerbs = []
        f.port.callResults = [:]
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        f.port.releaseCallSends()
        if let joining { #expect(await !joining.value) }
        await leaving?.value
        // Drain arrivals after old continuations, including an obsolete mixer accept.
        f.accept()
        let carol = JID(parsing: "carol@waddle.test/tablet")!
        f.store.handle(CallEvent(from: carol, sid: "barrier", kind: .propose(callAudio)))
        await eventually { f.port.callSends.last == .reject(carol, "barrier", false) }
        #expect(f.port.callSends == expected + [.propose(callPeer.bare, "g1", callVideo), .reject(carol, "barrier", false)])
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "g1", media: callVideo, initiator: callOwn)))
        #expect(f.store.lastError == nil)
        #expect(!f.store.isHandRaised)
        #expect(!f.store.isSelfMuteAdvertised)
        f.store.clear()
    }
}
