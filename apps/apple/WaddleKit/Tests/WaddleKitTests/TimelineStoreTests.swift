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

    @Test func roomAssignedIDIdentifiesTheSameMessageAcrossNicks() {
        let store = store()
        store.ingest(roomMessage("from bob", from: "bob", stanzaID: "same"))
        #expect(store.ingest(roomMessage("archive copy", from: "bob2", stanzaID: "same", at: date(1), source: .archive(mamID: "same"))) == .duplicate)
        #expect(items(store).count == 1)
        #expect(items(store)[0].body == "from bob")
    }

    @Test(arguments: [false, true], [false, true])
    func reusedOriginWithoutRoomIDKeepsBothAuthorsMessages(oldAuthorKnown: Bool, newAuthorKnown: Bool) {
        let store = store()
        var old = roomMessage("before handover", from: "sam", stanzaID: "unused", originID: "shared", at: date(1))
        old.identity = MessageIdentity(messageID: "shared", originID: "shared")
        old.authorRealJID = oldAuthorKnown ? bob : nil
        var incoming = roomMessage("after handover", from: "sam", stanzaID: "unused", originID: "shared")
        incoming.identity = old.identity
        incoming.authorRealJID = newAuthorKnown ? bare("erin@waddle.test") : nil

        store.ingest(old)
        guard case .inserted = store.ingest(incoming) else {
            Issue.record("a new holder of the nick must insert their own message")
            return
        }
        #expect(items(store).map(\.body) == ["before handover", "after handover"])
        #expect(items(store).first?.timestamp == date(1))
        #expect(items(store).first?.message.authorRealJID == old.authorRealJID)
        let rows = items(store)
        #expect(rows[0].presentationID != rows[1].presentationID)
    }

    @Test func localEchoIsSupersededByReflection() {
        let store = store()
        var echo = roomMessage("hello", from: "alice", stanzaID: "unused", originID: "client-1")
        echo.identity = MessageIdentity(messageID: "client-1", originID: "client-1")
        echo.authorRealJID = me.jid
        store.insertLocalEcho(echo, in: roomConversation)
        let presentationID = items(store)[0].presentationID
        #expect(items(store).first?.isLocalEcho == true)
        #expect(items(store).first?.actionTargetID == nil)

        var reflection = roomMessage("hello", from: "alice", stanzaID: "room-9", originID: "client-1")
        reflection.authorRealJID = me.jid
        store.ingest(reflection)
        let rows = items(store)
        #expect(rows.count == 1)
        #expect(rows[0].isLocalEcho == false)
        #expect(rows[0].id == "client-1")
        #expect(rows[0].actionTargetID == "room-9")
        #expect(rows[0].isMine)
        #expect(rows[0].presentationID == presentationID)
    }

    @Test(arguments: [false, true])
    func distinctRoomIDsNeverMergeOnReusedOrigin(sameAuthor: Bool) {
        let store = store()
        var original = roomMessage("original", from: "sam", stanzaID: "room-1", originID: "shared")
        original.authorRealJID = bob
        var incoming = roomMessage("new message", from: "sam", stanzaID: "room-2", originID: "shared")
        incoming.authorRealJID = sameAuthor ? bob : bare("erin@waddle.test")
        store.ingest(original)
        store.ingest(incoming)
        #expect(items(store).map(\.body) == ["original", "new message"])
    }

    @Test(arguments: [false, true])
    func unverifiedLocalEchoCannotBeClaimedByTheSameNick(conflictingAuthor: Bool) {
        let store = store()
        var echo = roomMessage("pending", from: me.nick, stanzaID: "unused", originID: "client-1")
        echo.identity = MessageIdentity(messageID: "client-1", originID: "client-1")
        echo.authorRealJID = me.jid
        store.insertLocalEcho(echo, in: roomConversation)
        var claim = roomMessage("claim", from: me.nick, stanzaID: "room-1", originID: "client-1")
        claim.authorRealJID = conflictingAuthor ? bob : nil
        store.ingest(claim)
        #expect(items(store).map(\.body) == ["pending", "claim"])
        #expect(items(store)[0].isLocalEcho)
    }

    @Test(arguments: [false, true])
    func matchingAuthorReconcilesCopiesWithoutRoomID(useOriginID: Bool) {
        let store = store()
        var original = roomMessage("hello", from: "bob", stanzaID: "unused", originID: "client-1", at: date(1), source: .archive(mamID: "mam-1"))
        original.identity = MessageIdentity(messageID: "client-1", originID: useOriginID ? "client-1" : nil)
        original.authorRealJID = bob
        var incoming = roomMessage("hello live", from: "bob2", stanzaID: "room-1", originID: "client-1")
        incoming.identity.originID = useOriginID ? "client-1" : nil
        incoming.authorRealJID = bob
        store.ingest(original)
        let presentationID = items(store)[0].presentationID
        #expect(store.ingest(incoming) == .duplicate)
        #expect(items(store).count == 1)
        #expect(items(store)[0].body == "hello live")
        #expect(items(store)[0].timestamp == date(1))
        #expect(items(store)[0].actionTargetID == "room-1")
        #expect(items(store)[0].presentationID == presentationID)
    }

    @Test func verifiedArchiveTwinAddsCanonicalIdentityToALiveRow() {
        let store = store()
        var live = roomMessage("live body", from: "bob", stanzaID: "unused", originID: "client-1")
        live.identity = MessageIdentity(messageID: "client-1", originID: "client-1")
        live.authorRealJID = bob
        store.ingest(live)
        store.ingest(reaction(["👍"], to: "room-1", from: "carol"))
        var archive = roomMessage("archived body", from: "bob", stanzaID: "room-1", originID: "client-1", at: date(1), source: .archive(mamID: "mam-1"))
        archive.authorRealJID = bob
        #expect(store.ingest(archive) == .duplicate)
        #expect(items(store)[0].actionTargetID == "room-1")
        #expect(store.ingest(roomMessage("unstamped replay", from: "bob2", stanzaID: "room-1")) == .duplicate)
        #expect(items(store).count == 1)
        #expect(items(store)[0].body == "live body")
        #expect(items(store)[0].timestamp == date(1))
        #expect(items(store)[0].message.authorRealJID == bob)
        #expect(items(store)[0].reactions.map(\.emoji) == ["👍"])
    }

    @Test func liveTwinWithoutRoomIDKeepsTheCanonicalArchiveIdentity() {
        let store = store()
        var archive = roomMessage("archive", from: "bob", stanzaID: "room-1", originID: "client-1", at: date(1), source: .archive(mamID: "mam-1"))
        archive.authorRealJID = bob
        store.ingest(archive)
        var live = roomMessage("live", from: "bob2", stanzaID: "unused", originID: "client-1")
        live.identity = MessageIdentity(messageID: "client-1", originID: "client-1")
        live.authorRealJID = bob
        #expect(store.ingest(live) == .duplicate)
        #expect(items(store)[0].actionTargetID == "room-1")
        #expect(items(store)[0].body == "live")
        #expect(items(store)[0].timestamp == date(1))
        #expect(store.ingest(roomMessage("replay", from: "bob", stanzaID: "room-1")) == .duplicate)
        #expect(items(store).count == 1)
    }

    @Test func foreignAuthorityAndAuthoredAliasesCannotClaimARoomIdentity() {
        let store = store()
        var original = roomMessage("original", from: "bob", stanzaID: "room-1", originID: "original-client-id")
        original.authorRealJID = bob
        store.ingest(original)
        var foreign = roomMessage("foreign id", from: "bob", stanzaID: "unused", originID: "other-client-id")
        foreign.identity = MessageIdentity(messageID: "other-client-id", originID: "other-client-id", stanzaID: StanzaID(id: "room-1", by: bare("evil.example")))
        foreign.authorRealJID = bob
        store.ingest(foreign)
        var alias = roomMessage("authored alias", from: "bob", stanzaID: "unused", originID: "room-1")
        alias.identity = MessageIdentity(messageID: "room-1", originID: "room-1")
        alias.authorRealJID = bob
        store.ingest(alias)
        #expect(items(store).map(\.body) == ["original", "foreign id", "authored alias"])
    }

    @Test func ambiguousAuthoredAliasCannotChooseBetweenCanonicalRows() {
        let store = store()
        for id in ["room-1", "room-2"] {
            var message = roomMessage(id, from: "bob", stanzaID: id, originID: "shared")
            message.authorRealJID = bob
            store.ingest(message)
        }
        var unknown = roomMessage("unresolved copy", from: "bob", stanzaID: "unused", originID: "shared")
        unknown.identity = MessageIdentity(messageID: "shared", originID: "shared")
        unknown.authorRealJID = bob
        store.ingest(unknown)
        #expect(items(store).map(\.body) == ["room-1", "room-2", "unresolved copy"])
    }

    @Test func canonicalLookupCannotBeHijackedByAnAuthoredPrimaryID() {
        let store = store()
        store.ingest(roomMessage("canonical", from: "bob", stanzaID: "room-1"))
        var claim = roomMessage("claim", from: "eve", stanzaID: "unused", originID: "room-1")
        claim.identity = MessageIdentity(messageID: "room-1", originID: "room-1")
        store.ingest(claim)
        let timeline = store.timeline(for: roomConversation)
        #expect(timeline.item(withID: "room-1")?.body == "canonical")
        for item in timeline.items {
            #expect(timeline.item(withPresentationID: item.presentationID)?.body == item.body)
        }
    }

    @Test func ambiguousAuthoredPrimaryIDsDoNotResolveToAnotherAuthorsRow() {
        let store = store()
        for nick in ["bob", "eve"] {
            var message = roomMessage(nick, from: nick, stanzaID: "unused", originID: "shared")
            message.identity = MessageIdentity(messageID: "shared", originID: "shared")
            store.ingest(message)
        }
        var canonical = roomMessage("canonical other", from: "carol", stanzaID: "other-room-id", originID: "shared")
        canonical.authorRealJID = bare("carol@waddle.test")
        store.ingest(canonical)
        #expect(store.timeline(for: roomConversation).item(withID: "shared") == nil)
        #expect(store.timeline(for: roomConversation).item(withID: "other-room-id")?.body == "canonical other")
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
