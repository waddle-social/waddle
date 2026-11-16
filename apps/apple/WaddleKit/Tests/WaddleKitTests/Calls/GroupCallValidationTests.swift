import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Group call validation") struct GroupCallValidationTests {
    @Test func validAccountWithOverlongMixerStillSupportsDirectCalls() async throws {
        let ownBare = try #require(BareJID(localpart: "alice", domain: String(repeating: "a", count: 1023)))
        let own = try #require(ownBare.with(resource: "phone"))
        let port = FakePort()
        let store = CallStore(port: port, ownJID: { own }, ownBareJID: ownBare, newSid: { "g1" })
        #expect(await !store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice"))
        #expect(store.state == .idle)
        #expect(port.callSends.isEmpty)
        #expect(store.lastError == "muji session initiate failed: invalid mixer jid")
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        #expect(port.callSends == [.propose(callPeer.bare, "g1", callAudio)])
        store.clear()
    }
}
