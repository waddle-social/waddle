import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// An archived room message in `thread`, hanging off `parent`.
private func archived(
    _ body: String,
    id: String,
    at seconds: TimeInterval,
    thread: String? = nil,
    parent: String? = nil
) -> WireMessage {
    var message = roomMessage(body, from: "carol", stanzaID: id, at: date(seconds), source: .archive(mamID: id))
    message.thread = thread
    message.parentThread = parent
    return message
}

private func page(_ messages: [WireMessage]) -> ArchivePage {
    ArchivePage(messages: messages, first: messages.first?.identity.stanzaID?.id, isComplete: false)
}

/// The composer sends only the thread id; the session works out which
/// thread a sub-thread hangs off and sends that as its XEP-0201 `parent`.
@MainActor
@Suite("Sub-thread sends")
struct SubThreadSendTests {
    private func online(_ port: FakePort = FakePort()) -> (SessionCoordinator, FakePort) {
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return (coordinator, port)
    }

    /// The room has "root" and its reply "r1" loaded.
    private func onlineWithThread(_ port: FakePort = FakePort()) -> (SessionCoordinator, FakePort) {
        let (coordinator, port) = online(port)
        coordinator.ingestArchive(page([
            archived("root", id: "root", at: 1),
            archived("r1", id: "r1", at: 2, thread: "root"),
        ]))
        return (coordinator, port)
    }

    @Test func aSubThreadSendCarriesItsParent() async throws {
        let (coordinator, port) = onlineWithThread()
        let id = try #require(await coordinator.send(Draft(text: "deeper", thread: "r1"), in: roomConversation))
        #expect(port.sent.count == 1)
        #expect(port.sent[0].options.thread == ThreadTarget(id: "r1", parent: "root"))
        let echo = try #require(coordinator.timelines.timeline(for: roomConversation).item(withID: id))
        #expect(echo.message.thread == "r1")
        #expect(echo.message.parentThread == "root")
    }

    @Test func aTopLevelThreadSendHasNoParent() async {
        let (coordinator, port) = onlineWithThread()
        await coordinator.send(Draft(text: "hi", thread: "root"), in: roomConversation)
        #expect(port.sent[0].options.thread == ThreadTarget(id: "root", parent: nil))
    }

    @Test func aStickerInASubThreadCarriesItsParent() async throws {
        let (coordinator, port) = onlineWithThread()
        let wave = Sticker(
            description: "wave",
            mediaType: "image/png",
            size: 2048,
            width: 512,
            height: 384,
            digests: FileDigests([.sha256: Data(repeating: 7, count: 32)]),
            sources: [try #require(URL(string: "https://upload.waddle.test/stickers/wave.png"))]
        )
        let pack = StickerPack(id: "pack-1", name: "Penguins", stickers: [wave])
        await coordinator.send(wave, from: pack, in: roomConversation, thread: "r1")
        #expect(port.sent[0].options.thread == ThreadTarget(id: "r1", parent: "root"))
    }

    @Test func anEditInASubThreadKeepsItsThreadAndParent() async throws {
        let (coordinator, port) = onlineWithThread()
        let id = try #require(await coordinator.send(Draft(text: "first", thread: "r1"), in: roomConversation))
        let item = try #require(coordinator.timelines.timeline(for: roomConversation).item(withID: id))
        #expect(await coordinator.edit(item, draft: Draft(text: "fixed")))
        #expect(port.corrections.count == 1)
        #expect(port.corrections[0].body == "fixed")
        #expect(port.corrections[0].options.thread == ThreadTarget(id: "r1", parent: "root"))
    }

    // MARK: Without the sub-thread's root

    /// Opened from the Inbox before its page arrives: only the live
    /// sub-thread messages, which carry their parent on the wire, are known.
    @Test func aSubThreadWhoseRootIsNotLoadedSendsTheParentItsMessagesCarry() async {
        let (coordinator, port) = online()
        var live = roomMessage("s1", from: "dave", stanzaID: "s1")
        live.thread = "r1"
        live.parentThread = "root"
        coordinator.route(live)
        #expect(coordinator.threadParent(of: "r1", in: roomConversation) == "root")

        await coordinator.send(Draft(text: "me too", thread: "r1"), in: roomConversation)
        #expect(port.sent[0].options.thread == ThreadTarget(id: "r1", parent: "root"))
    }

    /// The thread filter returns the newest page; in a long sub-thread its
    /// root, the oldest match, is not on it.
    @Test func aLongSubThreadWhoseRootIsOffItsPageStillKnowsItsParent() async {
        let port = FakePort()
        port.threadPages["r1"] = page((10..<60).map {
            archived("s\($0)", id: "s\($0)", at: TimeInterval($0), thread: "r1", parent: "root")
        })
        let (coordinator, _) = online(port)
        let thread = ThreadKey(room: room, threadID: "r1")
        await coordinator.openThread(thread)
        #expect(coordinator.threadHistory.history(for: thread)?.root == nil)
        #expect(coordinator.threadParent(of: "r1", in: roomConversation) == "root")

        await coordinator.send(Draft(text: "late", thread: "r1"), in: roomConversation)
        #expect(port.sent[0].options.thread == ThreadTarget(id: "r1", parent: "root"))
    }

    @Test func disagreeingMessagesGiveTheParentMostOfThemCarry() {
        let (coordinator, _) = online()
        coordinator.ingestArchive(page([
            archived("s1", id: "s1", at: 1, thread: "r1", parent: "other"),
            archived("s2", id: "s2", at: 2, thread: "r1", parent: "root"),
            archived("s3", id: "s3", at: 3, thread: "r1", parent: "root"),
            // A message cannot make a thread its own parent.
            archived("s4", id: "s4", at: 4, thread: "r1", parent: "r1"),
            archived("s5", id: "s5", at: 5, thread: "r1", parent: "r1"),
            archived("s6", id: "s6", at: 6, thread: "r1", parent: "r1"),
        ]))
        #expect(coordinator.threadParent(of: "r1", in: roomConversation) == "root")
    }

    @Test func aTieGoesToTheParentTheOldestMessageCarries() {
        let (coordinator, _) = online()
        coordinator.ingestArchive(page([
            archived("s1", id: "s1", at: 1, thread: "r1", parent: "other"),
            archived("s2", id: "s2", at: 2, thread: "r1", parent: "root"),
        ]))
        #expect(coordinator.threadParent(of: "r1", in: roomConversation) == "other")
    }

    /// Once the root is loaded, where it lives decides, whatever the
    /// messages claim.
    @Test func theRootOutranksWhatMessagesClaim() {
        let (coordinator, _) = onlineWithThread()
        coordinator.ingestArchive(page([
            archived("s1", id: "s1", at: 3, thread: "r1", parent: "other"),
        ]))
        #expect(coordinator.threadParent(of: "r1", in: roomConversation) == "root")
        #expect(coordinator.threadParent(of: "root", in: roomConversation) == nil)
    }

    @Test func aRetriedSendPicksUpAParentLearnedSince() async throws {
        let port = FakePort()
        port.sendOutcome = { _ in .rejected }
        let (coordinator, _) = online(port)
        let id = try #require(await coordinator.send(Draft(text: "again", thread: "r1"), in: roomConversation))
        #expect(port.sent[0].options.thread == ThreadTarget(id: "r1", parent: nil))
        #expect(coordinator.deliveries.state(of: id) == .failed)

        coordinator.ingestArchive(page([
            archived("root", id: "root", at: 1),
            archived("r1", id: "r1", at: 2, thread: "root"),
        ]))
        port.sendOutcome = { .sent(stanzaID: $0.clientID) }
        await coordinator.retry(clientID: id)
        #expect(port.sent.map(\.clientID) == [id, id])
        #expect(port.sent[1].options.thread == ThreadTarget(id: "r1", parent: "root"))
    }

    @Test func aRetriedSendKeepsTheParentItWasSentWith() async throws {
        let port = FakePort()
        port.sendOutcome = { _ in .rejected }
        let (coordinator, _) = onlineWithThread(port)
        let id = try #require(await coordinator.send(Draft(text: "again", thread: "r1"), in: roomConversation))
        port.sendOutcome = { .sent(stanzaID: $0.clientID) }
        await coordinator.retry(clientID: id)
        #expect(port.sent[1].options.thread == ThreadTarget(id: "r1", parent: "root"))
    }

    @Test func anEditOfAMessageSentWithoutItsParentAddsIt() async throws {
        let (coordinator, port) = onlineWithThread()
        var own = roomMessage("first", from: "alice", stanzaID: "m1", originID: "m1")
        own.authorRealJID = me.jid
        own.thread = "r1"
        coordinator.route(own)
        let item = try #require(coordinator.timelines.timeline(for: roomConversation).item(withID: "m1"))
        try #require(item.isMine)
        #expect(await coordinator.edit(item, draft: Draft(text: "fixed")))
        #expect(port.corrections[0].options.thread == ThreadTarget(id: "r1", parent: "root"))
    }
}
