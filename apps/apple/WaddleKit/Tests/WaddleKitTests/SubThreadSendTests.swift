import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

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

    @Test func aSubThreadSendCarriesItsParent() async throws {
        let (coordinator, port) = online()
        let id = try #require(await coordinator.send(Draft(text: "deeper", thread: "r1", parentThread: "root"), in: roomConversation))
        #expect(port.sent.count == 1)
        #expect(port.sent[0].options.thread == "r1")
        #expect(port.sent[0].options.parentThread == "root")
        let echo = try #require(coordinator.timelines.timeline(for: roomConversation).item(withID: id))
        #expect(echo.message.thread == "r1")
        #expect(echo.message.parentThread == "root")
    }

    @Test func aTopLevelThreadSendHasNoParent() async {
        let (coordinator, port) = online()
        await coordinator.send(Draft(text: "hi", thread: "root"), in: roomConversation)
        #expect(port.sent[0].options.thread == "root")
        #expect(port.sent[0].options.parentThread == nil)
    }

    @Test func aParentWithoutAThreadIsDropped() async {
        let (coordinator, port) = online()
        await coordinator.send(Draft(text: "hi", parentThread: "root"), in: roomConversation)
        #expect(port.sent[0].options.thread == nil)
        #expect(port.sent[0].options.parentThread == nil)
    }

    @Test func aRetriedSubThreadSendKeepsItsParent() async throws {
        let port = FakePort()
        port.sendOutcome = { _ in .rejected }
        let (coordinator, _) = online(port)
        let id = try #require(await coordinator.send(Draft(text: "again", thread: "r1", parentThread: "root"), in: roomConversation))
        #expect(coordinator.deliveries.state(of: id) == .failed)
        port.sendOutcome = { .sent(stanzaID: $0.clientID) }
        await coordinator.retry(clientID: id)
        #expect(port.sent.map(\.clientID) == [id, id])
        #expect(port.sent[1].options.thread == "r1")
        #expect(port.sent[1].options.parentThread == "root")
    }

    @Test func aStickerInASubThreadCarriesItsParent() async throws {
        let (coordinator, port) = online()
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
        await coordinator.send(wave, from: pack, in: roomConversation, thread: "r1", parentThread: "root")
        #expect(port.sent[0].options.thread == "r1")
        #expect(port.sent[0].options.parentThread == "root")
    }

    @Test func anEditInASubThreadKeepsItsThreadAndParent() async throws {
        let (coordinator, port) = online()
        let id = try #require(await coordinator.send(Draft(text: "first", thread: "r1", parentThread: "root"), in: roomConversation))
        let item = try #require(coordinator.timelines.timeline(for: roomConversation).item(withID: id))
        #expect(await coordinator.edit(item, draft: Draft(text: "fixed")))
        #expect(port.corrections.count == 1)
        #expect(port.corrections[0].body == "fixed")
        #expect(port.corrections[0].options.thread == "r1")
        #expect(port.corrections[0].options.parentThread == "root")
    }
}
