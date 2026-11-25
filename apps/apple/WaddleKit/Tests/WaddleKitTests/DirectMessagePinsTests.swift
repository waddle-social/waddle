import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// Pins in a conversation of two (`urn:waddle:pin:0`), as web has them:
/// either person may pin, both are told, and the server names a pinned
/// message by its `<message id>`, the one id both people have for it.
@MainActor
@Suite("Direct message pins")
struct DirectMessagePinsTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    private let bobFull = jid("bob@waddle.test/laptop")
    private let meBare = JID(bare: me.jid, resource: nil)!
    private let bobBare = JID(bare: bob, resource: nil)!

    /// What the server sends both people: a chat message from the pinner to the other, with words in it.
    private func pinEvent(_ action: PinEvent.Action, target: String, by pinner: JID, to other: JID) -> WireMessage {
        var message = directMessage("\(pinner.bare) pinned a message", from: pinner, to: other, id: UUID().uuidString)
        let preview = PinPreview(author: bobBare, authorNick: nil, text: "lunch at noon", messageTimestamp: nil)
        message.pinEvent = PinEvent(action: action, targetStanzaID: target, by: pinner, preview: action == .pinned ? preview : nil)
        return message
    }

    private func row(_ id: String, in coordinator: SessionCoordinator) throws -> TimelineItem {
        try #require(coordinator.timelines.timeline(for: bobConversation).item(withID: id))
    }

    @Test func eitherPersonMayPin() {
        let (coordinator, _) = online()
        #expect(coordinator.pinAccess(in: bobConversation) == .allowed)
    }

    @Test func aMessageInTheConversationOffersPinAndAPinnedOneUnpin() throws {
        let (coordinator, _) = online()
        coordinator.route(directMessage("lunch at noon", from: bobFull, to: meBare, id: "wire-1"))
        let item = try row("wire-1", in: coordinator)
        func offered() -> [MessageAction] {
            MessageActions.available(for: item, in: MessageActionContext(
                thread: nil, replyCount: 0, isPinned: coordinator.isPinned(item), pinning: coordinator.pinAccess(in: bobConversation)
            ))
        }
        #expect(offered().contains(.pin))
        coordinator.route(pinEvent(.pinned, target: "wire-1", by: meBare, to: bobBare))
        #expect(offered().contains(.unpin))
        #expect(!offered().contains(.pin))
    }

    /// The server keeps no pins with a person on another server, and asking for them would go to that server.
    @Test func aConversationWithSomeoneElsewhereHasNoPins() async {
        let (coordinator, port) = online()
        let away = ConversationID.direct(jid("dana@elsewhere.example").bare)
        port.pinsByConversation[away.jid] = [PinEntry(
            targetStanzaID: "x", pinner: nil, pinnedAt: nil,
            preview: PinPreview(author: nil, authorNick: nil, text: "", messageTimestamp: nil)
        )]
        #expect(coordinator.pinAccess(in: away) == .denied)

        await coordinator.open(away, screen: ConversationScreenID())
        #expect(coordinator.pins.pins(in: away.jid).isEmpty, "never asked for")

        coordinator.route(pinEvent(.pinned, target: "x", by: jid("dana@elsewhere.example"), to: meBare))
        #expect(coordinator.pins.pins(in: away.jid).isEmpty)
    }

    /// The author took a pinned message back: the server unpins it and says so.
    @Test func aRetractedMessagesPinGoesWithIt() throws {
        let (coordinator, _) = online()
        coordinator.route(directMessage("lunch at noon", from: bobFull, to: meBare, id: "wire-1"))
        coordinator.route(pinEvent(.pinned, target: "wire-1", by: meBare, to: bobBare))
        coordinator.route(pinEvent(.unpinned, target: "wire-1", by: bobBare, to: meBare))
        #expect(coordinator.pins.pins(in: bob).isEmpty)
        #expect(!coordinator.isPinned(try row("wire-1", in: coordinator)))
    }

    @Test func aPinOfAMessageNotLoadedIsListedWithItsPreview() {
        let (coordinator, _) = online()
        coordinator.route(pinEvent(.pinned, target: "older", by: bobBare, to: meBare))
        #expect(coordinator.pins.pins(in: bob).first?.preview.text == "lunch at noon")
    }

    @Test func aRoomsPinNoticeInItsHistoryStaysARow() {
        let (coordinator, _) = online()
        var notice = roomMessage("bob pinned a message", from: "general", stanzaID: "n1")
        notice.pinEvent = PinEvent(action: .pinned, targetStanzaID: "s1", by: bobBare, preview: nil)
        notice.source = .archive(mamID: "mam-1")
        coordinator.ingestArchive(ArchivePage(messages: [notice], first: nil, isComplete: true))
        #expect(coordinator.timelines.timeline(for: roomConversation).items.count == 1)
    }

    @Test func aPinIsAskedForByTheMessagesOwnIDNotItsOriginID() async throws {
        let (coordinator, port) = online()
        var message = directMessage("lunch at noon", from: bobFull, to: meBare, id: "wire-1", archiveID: "mine-1")
        // The server looks a pin up by `<message id>` or an archive id, never by origin-id.
        message.identity.originID = "origin-1"
        coordinator.route(message)

        #expect(await coordinator.setPinned(true, try row("wire-1", in: coordinator)))
        #expect(port.pinWrites.map(\.targetID) == ["wire-1"])
        #expect(port.pinWrites.first?.conversation == bobConversation)
        #expect(port.pinWrites.first?.pinned == true)
    }

    @Test func aPinTheOtherPersonMadeMarksTheMessageAndIsNoRowOrUnread() throws {
        let (coordinator, _) = online()
        coordinator.route(directMessage("lunch at noon", from: bobFull, to: meBare, id: "wire-1", archiveID: "mine-1"))
        let unread = coordinator.unread.count(for: bobConversation)
        #expect(!coordinator.isPinned(try row("wire-1", in: coordinator)))

        coordinator.route(pinEvent(.pinned, target: "wire-1", by: bobBare, to: meBare))

        #expect(coordinator.isPinned(try row("wire-1", in: coordinator)))
        #expect(coordinator.pins.pins(in: bob).map(\.targetStanzaID) == ["wire-1"])
        #expect(coordinator.pins.pins(in: bob).first?.preview.text == "lunch at noon")
        #expect(coordinator.timelines.timeline(for: bobConversation).items.count == 1, "the notice is not a message")
        #expect(coordinator.unread.count(for: bobConversation) == unread)
    }

    /// The server sends the pinner the same message it sends the other: from us, to them.
    @Test func ourOwnPinComesBackUnderTheOtherPersonsConversation() throws {
        let (coordinator, _) = online()
        coordinator.route(directMessage("lunch at noon", from: bobFull, to: meBare, id: "wire-1"))

        coordinator.route(pinEvent(.pinned, target: "wire-1", by: meBare, to: bobBare))
        #expect(coordinator.isPinned(try row("wire-1", in: coordinator)))

        coordinator.route(pinEvent(.unpinned, target: "wire-1", by: meBare, to: bobBare))
        #expect(!coordinator.isPinned(try row("wire-1", in: coordinator)))
        #expect(coordinator.pins.pins(in: bob).isEmpty)
    }

    @Test func aMessagePinnedByItsArchiveIDIsStillMarked() throws {
        let (coordinator, _) = online()
        coordinator.route(directMessage("lunch at noon", from: bobFull, to: meBare, id: "wire-1", archiveID: "mine-1"))
        coordinator.route(pinEvent(.pinned, target: "mine-1", by: bobBare, to: meBare))
        #expect(coordinator.isPinned(try row("wire-1", in: coordinator)))
    }

    @Test func openingTheConversationReadsItsPins() async {
        let (coordinator, port) = online()
        let preview = PinPreview(author: bobBare, authorNick: nil, text: "lunch at noon", messageTimestamp: nil)
        port.pinsByConversation[bob] = [PinEntry(targetStanzaID: "wire-1", pinner: bobBare, pinnedAt: nil, preview: preview)]

        await coordinator.open(bobConversation, screen: ConversationScreenID())

        #expect(coordinator.pins.pins(in: bob).map(\.targetStanzaID) == ["wire-1"])
    }

    @Test func aPinNoticeInTheArchiveIsNoRow() {
        let (coordinator, _) = online()
        var notice = pinEvent(.pinned, target: "wire-1", by: bobBare, to: meBare)
        notice.source = .archive(mamID: "mam-1")
        coordinator.ingestArchive(ArchivePage(messages: [notice], first: nil, isComplete: true))
        #expect(coordinator.timelines.timeline(for: bobConversation).items.isEmpty)
    }
}
