import Foundation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor struct CallChatTests {
    private func store() -> TimelineStore {
        let store = TimelineStore()
        store.account = me
        return store
    }

    private func announcement(_ origin: String, thread: String, started: Date) -> WireMessage {
        var message = WireMessage(
            type: .groupchat, from: jid(room.description), to: JID(bare: me.jid, resource: "phone"),
            identity: MessageIdentity(messageID: "m-\(origin)", originID: origin, stanzaIDs: [StanzaID(id: "s-\(origin)", by: room)]),
            body: "ada started a call", thread: thread
        )
        message.callThread = CallThreadAnchor(kind: .room, hasVideo: false, started: started)
        return message
    }

    private func end(_ origin: String) -> WireMessage {
        var message = WireMessage(
            type: .groupchat, from: jid(room.description), to: JID(bare: me.jid, resource: "phone"),
            identity: MessageIdentity(messageID: "e-\(origin)", stanzaIDs: [StanzaID(id: "es-\(origin)", by: room)])
        )
        message.callThreadEnded = CallThreadEnded(anchorID: origin, duration: 60)
        return message
    }

    private func chat(_ body: String, from nick: String, id: String, thread: String) -> WireMessage {
        var message = roomMessage(body, from: nick, stanzaID: id)
        message.thread = thread
        return message
    }

    private let morning = Date(timeIntervalSince1970: 1_800_000_000)

    @Test func theChatIsTheThreadOfTheCallGoingOn() {
        let store = store()
        store.ingest(announcement("o1", thread: "t1", started: morning))
        store.ingest(end("o1"))
        store.ingest(announcement("o2", thread: "t2", started: morning.addingTimeInterval(3600)))
        #expect(CallChat.thread(in: store.timeline(for: roomConversation).items) == "t2")
    }

    @Test func aCallTheRoomSaidIsOverHasNoChatToOffer() {
        let store = store()
        store.ingest(announcement("o1", thread: "t1", started: morning))
        store.ingest(end("o1"))
        #expect(CallChat.thread(in: store.timeline(for: roomConversation).items) == nil)
        #expect(CallChat.thread(in: []) == nil)
    }

    @Test func messagesInTheCallsThreadAreItsChat() {
        let store = store()
        store.ingest(announcement("o1", thread: "t1", started: morning))
        store.ingest(chat("hello", from: "bob", id: "c1", thread: "t1"))
        store.ingest(roomMessage("in the room", from: "bob", stanzaID: "r1"))
        let timeline = store.timeline(for: roomConversation)
        #expect(timeline.threadReplies(threadID: "t1").map(\.body) == ["hello"])
        #expect(timeline.feedItems.map(\.body) == ["ada started a call", "in the room"], "chat stays out of the room's feed")
    }

    @Test func onlyOtherPeoplesMessagesAfterTheLastSeenAreUnread() {
        let store = store()
        store.ingest(announcement("o1", thread: "t1", started: morning))
        store.ingest(chat("one", from: "bob", id: "c1", thread: "t1"))
        store.ingest(chat("two", from: "bob", id: "c2", thread: "t1"))
        store.ingest(chat("mine", from: me.nick, id: "c3", thread: "t1"))
        store.ingest(chat("three", from: "carol", id: "c4", thread: "t1"))
        let replies = store.timeline(for: roomConversation).threadReplies(threadID: "t1")
        #expect(replies.count == 4)
        #expect(CallChat.unread(in: replies, after: nil) == 3, "our own message is not unread")
        #expect(CallChat.unread(in: replies, after: replies[0].id) == 2)
        #expect(CallChat.unread(in: replies, after: replies[3].id) == 0)
        #expect(CallChat.unread(in: replies, after: "gone") == 3, "what was seen is no longer loaded: all of it is new")
        #expect(CallChat.unread(in: [], after: nil) == 0)
    }

    @Test func whatWasSeenIsKeptForTheCallAndForgottenWithIt() {
        var memory = CallStageMemory()
        memory.follow(call: "g1")
        memory.sawChat(upTo: "c2")
        memory.sawChat(upTo: nil)
        #expect(memory.chatSeenID == "c2", "an empty chat takes nothing back")
        memory.follow(call: "g2")
        #expect(memory.chatSeenID == nil)
    }

    /// The room's announcement heads the chat, and nobody wrote it.
    @Test func aCallsChatIsNotStartedByTheRoom() {
        let store = store()
        store.ingest(announcement("o1", thread: "t1", started: morning))
        store.ingest(chat("hello", from: "bob", id: "c1", thread: "t1"))
        let timeline = store.timeline(for: roomConversation)
        let header = ThreadHeader(
            root: timeline.items.first { $0.callAnchor != nil }, replies: timeline.threadReplies(threadID: "t1"), replyCount: 1,
            breadcrumb: []
        )
        #expect(header.startedBy == nil)
        #expect(header.faces.map(\.name) == ["bob"])
    }

    @Test func theWords() {
        #expect(CallChat.buttonTitle(unread: 0, isOpen: false) == "Call chat")
        #expect(CallChat.buttonTitle(unread: 3, isOpen: false) == "Call chat, 3 unread")
        #expect(CallChat.buttonTitle(unread: 3, isOpen: true) == "Hide call chat")
        #expect(CallChat.linkTitle(messages: 0) == "Call chat")
        #expect(CallChat.linkTitle(messages: 1) == "1 message in call chat")
        #expect(CallChat.linkTitle(messages: 12) == "12 messages in call chat")
        #expect(CallChat.badge(0) == nil)
        #expect(CallChat.badge(7) == "7")
        #expect(CallChat.badge(250) == "99+")
    }
}
