import Foundation
import Testing
@testable import WaddleKit

/// Room identity trust (XEP-0359), groupchat routing and archived
/// XEP-0424 tombstones.
@MainActor
@Suite("Trust and tombstones")
struct TrustAndTombstoneTests {
    private func store() -> TimelineStore {
        let store = TimelineStore()
        store.account = me
        return store
    }

    /// A live room row whose only id is a stanza-id from another authority
    /// has no trustworthy key and is not shown.
    @Test func foreignStanzaIDIsNeverARoomKey() {
        let timeline = store()
        var message = roomMessage("spoof", from: "eve", stanzaID: "x")
        let foreign = StanzaID(id: "victim-id", by: bare("evil.example"))
        message.identity = MessageIdentity(stanzaID: foreign, stanzaIDs: [foreign])
        #expect(timeline.ingest(message) == .ignored)
        #expect(timeline.timeline(for: roomConversation).items.isEmpty)
    }

    @Test func archivedRoomRowFallsBackToItsMAMID() {
        let timeline = store()
        var message = roomMessage("old", from: "bob", stanzaID: "x", source: .archive(mamID: "mam-7"))
        message.identity = MessageIdentity()
        timeline.ingest(message)
        #expect(timeline.timeline(for: roomConversation).items.map(\.id) == ["mam-7"])
    }

    @Test func groupchatWithoutSenderIsNotRouted() {
        #expect(me.route(from: nil, to: JID(bare: me.jid, resource: "phone"), isGroupchat: true) == nil)
    }

    @Test func bodylessArchivedTombstoneIsShown() {
        let timeline = store()
        var tombstone = roomMessage(nil, from: "bob", stanzaID: "s1", source: .archive(mamID: "s1"))
        tombstone.isRetracted = true
        timeline.ingest(tombstone)
        let rows = timeline.timeline(for: roomConversation).items
        #expect(rows.map(\.id) == ["s1"])
        #expect(rows.first?.tombstone == .retracted)
    }

    @Test func archivedTombstoneMarksALoadedRow() {
        let timeline = store()
        timeline.ingest(roomMessage("hello", from: "bob", stanzaID: "s1"))
        var tombstone = roomMessage(nil, from: "bob", stanzaID: "s1", source: .archive(mamID: "s1"))
        tombstone.isRetracted = true
        timeline.ingest(tombstone)
        let rows = timeline.timeline(for: roomConversation).items
        #expect(rows.count == 1)
        #expect(rows.first?.tombstone == .retracted)
    }
}
