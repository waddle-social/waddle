import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct RemoteMediaAuthorshipTests {
    private let home = RemoteMediaPolicy(account: me.jid)

    @Test func aForeignRoomStampDoesNotProveAMessageIsOurs() {
        let row = roomRow(in: foreignRoom, sender: me.jid, isMine: true)
        #expect(home.decision(for: row) == .withheld(host: Host("chat.remote.test")))
    }

    @Test func anOwnLocalEchoLoadsInAForeignRoom() {
        var row = roomRow(in: foreignRoom, isMine: true)
        row.isLocalEcho = true
        #expect(home.decision(for: row) == .allowed)
    }

    @Test func aSentCarbonFromAnotherDeviceLoadsInAForeignDM() throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        // The bridge unwraps carbons into live stanzas before the reducer sees them.
        session.timelines.ingest(directMessage(
            imageURL, from: jid("alice@waddle.test/laptop"), to: jid("bob@remote.test"), id: "carbon"
        ))
        let row = try #require(session.timelines.timeline(for: .direct(foreignSender)).items.first)
        #expect(row.isMine)
        #expect(session.remoteMediaDecision(for: row) == .allowed)
    }

    @Test func sharingTheConfiguredNickDoesNotProveASenderIsUs() {
        let impostor = roomRow(in: foreignRoom, sender: foreignSender, isMine: true)
        #expect(home.decision(for: impostor) == .withheld(host: Host("chat.remote.test")))
    }

    @Test func aDifferentlyStampedCorrectionIsAcceptedButItsMediaWaits() throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        let conversation = ConversationID.room(bare("dev@waddle.test"))
        let original = roomRow(in: conversation, sender: bob).message
        session.timelines.ingest(original)
        var correction = original
        correction.identity = MessageIdentity(messageID: "edit", originID: "edit")
        correction.replacesID = "message"
        correction.authorRealJID = foreignSender
        correction.body = "https://images.example/replacement.gif"
        session.timelines.ingest(correction)

        let row = try #require(session.timelines.timeline(for: conversation).items.first)
        #expect(row.body == correction.body)
        #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("remote.test")))
    }

    @Test func aKnownForeignRevisionOfAnAnonymousRowWaits() throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        let conversation = ConversationID.room(bare("dev@waddle.test"))
        let original = roomRow(in: conversation).message
        session.timelines.ingest(original)
        var correction = original
        correction.identity = MessageIdentity(messageID: "edit", originID: "edit")
        correction.replacesID = "message"
        correction.authorRealJID = foreignSender
        correction.body = "https://images.example/replacement.gif"
        session.timelines.ingest(correction)

        let row = try #require(session.timelines.timeline(for: conversation).items.first)
        #expect(row.body == correction.body)
        #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("remote.test")))

        correction.identity = MessageIdentity(messageID: "second-edit", originID: "second-edit")
        correction.authorRealJID = nil
        correction.body = "https://images.example/second-replacement.gif"
        session.timelines.ingest(correction)
        let editedAgain = try #require(session.timelines.timeline(for: conversation).items.first)
        #expect(editedAgain.body == correction.body)
        // An earlier editor's stamp does not describe this unstamped revision.
        #expect(session.remoteMediaDecision(for: editedAgain) == .allowed)
    }

    @Test func nickTakeoverDoesNotInheritLocalEchoOrAnEarlierCorrectionStamp() throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        var original = roomRow(in: foreignRoom, sender: me.jid, isMine: true).message
        original.from = foreignRoom.jid.with(resource: me.nick)
        session.timelines.insertLocalEcho(original, in: foreignRoom)
        let timeline = session.timelines.timeline(for: foreignRoom)
        #expect(session.remoteMediaDecision(for: try #require(timeline.items.first)) == .allowed)

        var correction = original
        correction.replacesID = "message"
        correction.identity = MessageIdentity(messageID: "edit", originID: "edit")
        correction.body = "https://images.example/replacement.gif"
        session.timelines.ingest(correction)
        #expect(session.remoteMediaDecision(for: try #require(timeline.items.first)) == .withheld(host: Host("chat.remote.test")))

        correction.identity = MessageIdentity(messageID: "second-edit")
        correction.authorRealJID = nil
        correction.body = "https://images.example/takeover.gif"
        session.timelines.ingest(correction)
        let edited = try #require(timeline.items.first)
        #expect(edited.body == correction.body)
        #expect(session.remoteMediaDecision(for: edited) == .withheld(host: Host("chat.remote.test")))
    }

    @Test func foreignRoomReflectionMustMatchTheLocalSendAndItsMedia() async throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        let clientID = try #require(await session.send(Draft(text: imageURL), in: foreignRoom))
        let timeline = session.timelines.timeline(for: foreignRoom)
        #expect(session.remoteMediaDecision(for: try #require(timeline.items.first)) == .allowed)

        var reflection = roomRow(in: foreignRoom, sender: me.jid, isMine: true).message
        reflection.from = foreignRoom.jid.with(resource: me.nick)
        reflection.identity = MessageIdentity(messageID: clientID, originID: clientID)
        reflection.body = "https://images.example/forged.gif"
        session.timelines.ingest(reflection)
        let forged = try #require(timeline.items.first)
        #expect(!forged.isLocalEcho)
        #expect(forged.body == reflection.body)
        #expect(session.remoteMediaDecision(for: forged) == .withheld(host: Host("chat.remote.test")))

        // A separate copy need not name a real JID to match a local send.
        reflection.authorRealJID = nil
        reflection.body = imageURL
        session.timelines.ingest(reflection)
        let matched = try #require(timeline.items.last)
        #expect(!matched.isLocalEcho)
        #expect(matched.body == imageURL)
        #expect(session.remoteMediaDecision(for: matched) == .allowed)
    }

    @Test func ourOwnForeignRoomCorrectionLoadsAfterItsReflection() async throws {
        let port = FakePort()
        let session = SessionCoordinator(account: me, port: port)
        var original = roomRow(in: foreignRoom, sender: me.jid, isMine: true).message
        original.from = foreignRoom.jid.with(resource: me.nick)
        session.timelines.ingest(original)
        let timeline = session.timelines.timeline(for: foreignRoom)
        let row = try #require(timeline.items.first)
        let replacement = "https://images.example/my-edit.gif"
        #expect(await session.edit(row, draft: Draft(text: replacement)))

        var correction = original
        correction.replacesID = "message"
        correction.identity = MessageIdentity(messageID: "correction", originID: "correction")
        correction.authorRealJID = nil
        correction.body = replacement
        session.timelines.ingest(correction)
        let edited = try #require(timeline.items.first)
        #expect(edited.body == replacement)
        #expect(session.remoteMediaDecision(for: edited) == .allowed)

        correction.body = "https://images.example/replaced.gif"
        session.timelines.ingest(correction)
        let altered = try #require(timeline.items.first)
        #expect(altered.body == correction.body)
        #expect(session.remoteMediaDecision(for: altered) == .withheld(host: Host("chat.remote.test")))
    }

    @Test func aLocalCaptionEditDoesNotApproveAnInheritedAttachment() async throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        var original = roomRow(in: foreignRoom, sender: me.jid, isMine: true).message
        original.from = foreignRoom.jid.with(resource: me.nick)
        original.body = "Original caption"
        original.sharedFiles = [SharedFile(url: mediaURL("unapproved.png"), mediaType: "image/png")]
        session.timelines.ingest(original)
        let timeline = session.timelines.timeline(for: foreignRoom)
        let row = try #require(timeline.items.first)
        #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("chat.remote.test")))
        #expect(await session.edit(row, draft: Draft(text: "Edited caption")))

        var correction = original
        correction.replacesID = "message"
        correction.identity = MessageIdentity(messageID: "correction", originID: "correction")
        correction.body = "Edited caption"
        session.timelines.ingest(correction)
        let edited = try #require(timeline.items.first)
        #expect(edited.body == correction.body)
        #expect(session.remoteMediaDecision(for: edited) == .withheld(host: Host("chat.remote.test")))
    }
}
