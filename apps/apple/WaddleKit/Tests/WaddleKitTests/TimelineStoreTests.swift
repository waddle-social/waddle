import Foundation
import Testing
@testable import WaddleKit

@MainActor
@Suite("Timeline reducer")
struct TimelineStoreTests {
    private func store() -> TimelineStore {
        let store = TimelineStore()
        store.account = me
        return store
    }

    private func items(_ store: TimelineStore, _ conversation: ConversationID = roomConversation) -> [TimelineItem] {
        store.timeline(for: conversation).items
    }

    @Test func insertsAndOrdersByTimestampWithLiveLast() {
        let store = store()
        store.ingest(roomMessage("live", from: "bob", stanzaID: "s3"))
        store.ingest(roomMessage("old", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")))
        store.ingest(roomMessage("newer", from: "bob", stanzaID: "s2", at: date(2), source: .archive(mamID: "s2")))
        #expect(items(store).map(\.body) == ["old", "newer", "live"])
    }

    @Test func dedupesReplayAndArchiveCopy() {
        let store = store()
        let first = store.ingest(roomMessage("hi", from: "bob", stanzaID: "s1"))
        let replay = store.ingest(roomMessage("hi", from: "bob", stanzaID: "s1"))
        let archived = store.ingest(roomMessage("hi", from: "bob", stanzaID: "s1", at: date(5), source: .archive(mamID: "s1")))
        guard case .inserted = first else { Issue.record("expected insert"); return }
        #expect(replay == .duplicate)
        #expect(archived == .duplicate)
        #expect(items(store).count == 1)
        // The archive copy brings the server timestamp to the live row.
        #expect(items(store).first?.timestamp == date(5))
    }

    @Test func crossSenderCollisionStaysDistinct() {
        let store = store()
        store.ingest(roomMessage("from bob", from: "bob", stanzaID: "same"))
        store.ingest(roomMessage("from eve", from: "eve", stanzaID: "same"))
        #expect(items(store).count == 2)
    }

    @Test func localEchoIsSupersededByReflection() {
        let store = store()
        var echo = roomMessage("hello", from: "alice", stanzaID: "unused", originID: "client-1")
        echo.identity = MessageIdentity(messageID: "client-1", originID: "client-1")
        store.insertLocalEcho(echo, in: roomConversation)
        #expect(items(store).first?.isLocalEcho == true)
        #expect(items(store).first?.actionTargetID == nil)

        store.ingest(roomMessage("hello", from: "alice", stanzaID: "room-9", originID: "client-1"))
        let rows = items(store)
        #expect(rows.count == 1)
        #expect(rows[0].isLocalEcho == false)
        #expect(rows[0].id == "client-1")
        #expect(rows[0].actionTargetID == "room-9")
        #expect(rows[0].isMine)
    }

    @Test func reactionsReplaceTheSendersSet() {
        let store = store()
        store.ingest(roomMessage("msg", from: "bob", stanzaID: "s1"))
        store.ingest(reaction(["👍", "🎉"], to: "s1", from: "carol"))
        store.ingest(reaction(["👍"], to: "s1", from: "dave"))
        store.ingest(reaction(["🎉"], to: "s1", from: "carol"))
        let reactions = items(store)[0].reactions
        #expect(reactions.map(\.emoji) == ["👍", "🎉"])
        #expect(reactions.first { $0.emoji == "👍" }?.count == 1)
        #expect(reactions.first { $0.emoji == "🎉" }?.reactors == ["carol"])
    }

    @Test func olderArchivedReactionDoesNotOverrideNewerClear() {
        let store = store()
        store.ingest(roomMessage("msg", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")))
        store.ingest(reaction([], to: "s1", from: "carol", at: date(10)))
        store.ingest(reaction(["👍"], to: "s1", from: "carol", at: date(5)))
        #expect(items(store)[0].reactions.isEmpty)
    }

    @Test func mutationBeforeTargetIsParkedThenApplied() {
        let store = store()
        store.ingest(reaction(["❤️"], to: "s1", from: "carol", at: date(3)))
        store.ingest(roomMessage("msg", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")))
        #expect(items(store)[0].reactions.map(\.emoji) == ["❤️"])
    }

    @Test func onlyTheAuthorCanCorrectOrRetract() {
        let store = store()
        store.ingest(roomMessage("original", from: "bob", stanzaID: "s1", originID: "o1"))

        var spoof = roomMessage("hacked", from: "eve", stanzaID: "s2")
        spoof.replacesID = "o1"
        store.ingest(spoof)
        #expect(items(store)[0].body == "original")

        var correction = roomMessage("fixed", from: "bob", stanzaID: "s3")
        correction.replacesID = "o1"
        store.ingest(correction)
        #expect(items(store)[0].body == "fixed")
        #expect(items(store)[0].isEdited)

        var spoofRetract = roomMessage(nil, from: "eve", stanzaID: "s4")
        spoofRetract.retractsID = "s1"
        store.ingest(spoofRetract)
        #expect(items(store)[0].tombstone == nil)

        var retract = roomMessage(nil, from: "bob", stanzaID: "s5")
        retract.retractsID = "s1"
        store.ingest(retract)
        #expect(items(store)[0].tombstone == .retracted)
    }

    @Test func onlyTheRoomCanModerate() {
        let store = store()
        store.ingest(roomMessage("spam", from: "bob", stanzaID: "s1"))

        var occupantClaim = roomMessage(nil, from: "eve", stanzaID: "s2")
        occupantClaim.moderation = .init(targetID: "s1", moderatedBy: "eve", reason: nil)
        store.ingest(occupantClaim)
        #expect(items(store)[0].tombstone == nil)

        var moderation = WireMessage(
            type: .groupchat,
            from: JID(bare: room, resource: nil),
            to: nil,
            identity: MessageIdentity(stanzaID: StanzaID(id: "s3", by: room))
        )
        moderation.moderation = .init(targetID: "s1", moderatedBy: "general@muc.waddle.test/mod", reason: "spam")
        store.ingest(moderation)
        #expect(items(store)[0].tombstone == .moderated(by: "general@muc.waddle.test/mod", reason: "spam"))
    }

    @Test func directMessagesRouteToPeerAndDedupeOwnEcho() {
        let store = store()
        let echo = directMessage("hey", from: JID(bare: me.jid, resource: nil)!, to: JID(bare: bob, resource: nil)!, id: "c1")
        store.insertLocalEcho(echo, in: bobConversation)
        let archived = directMessage(
            "hey",
            from: jid("alice@waddle.test/phone"),
            to: jid("bob@waddle.test"),
            id: "c1",
            archiveID: "a1",
            at: date(1),
            source: .archive(mamID: "a1")
        )
        #expect(store.ingest(archived) == .duplicate)
        let rows = items(store, bobConversation)
        #expect(rows.count == 1)
        #expect(rows[0].isLocalEcho == false)
        // 1:1 actions target the author-assigned id, never our archive id.
        #expect(rows[0].actionTargetID == "c1")
    }

    @Test func threadRepliesStayOutOfTheFeed() {
        let store = store()
        store.ingest(roomMessage("root", from: "bob", stanzaID: "root"))
        var reply = roomMessage("reply", from: "carol", stanzaID: "r1")
        reply.thread = "root"
        store.ingest(reply)
        let timeline = store.timeline(for: roomConversation)
        #expect(timeline.feedItems.map(\.body) == ["root"])
        #expect(timeline.threadReplies(threadID: "root").map(\.body) == ["reply"])
        #expect(timeline.replyCount(for: timeline.feedItems[0]) == 1)
    }

    @Test func liveInsertsTrimOldestButHistoryDoesNot() {
        let store = TimelineStore(maxItemsPerConversation: 3)
        store.account = me
        for index in 0..<5 {
            store.ingest(roomMessage("h\(index)", from: "bob", stanzaID: "h\(index)", at: date(Double(index)), source: .archive(mamID: "h\(index)")))
        }
        #expect(items(store).count == 5)
        store.ingest(roomMessage("live", from: "bob", stanzaID: "live"))
        #expect(items(store).map(\.body) == ["h3", "h4", "live"])
    }

    @Test func replyFallbackIsStrippedFromBody() {
        let store = store()
        var message = roomMessage("> quoted\n\nanswer", from: "bob", stanzaID: "s1")
        message.reply = .init(id: "p", author: nil, fallback: 0..<10)
        store.ingest(message)
        #expect(items(store)[0].body == "answer")
    }
}
