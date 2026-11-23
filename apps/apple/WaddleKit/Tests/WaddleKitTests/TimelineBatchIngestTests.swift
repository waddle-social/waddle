import Foundation
import Observation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Timeline batch ingest")
struct TimelineBatchIngestTests {
    private let receivedAt = date(1_000)

    private func store() -> TimelineStore {
        let store = TimelineStore()
        store.account = me
        return store
    }

    /// What a row renders; `presentationID` is a fresh UUID per row, so
    /// two stores never agree on it.
    private struct Rendered: Equatable {
        let id: String
        let body: String
        let timestamp: Date?
        let reactions: [ReactionGroup]
        let tombstone: Tombstone?
        let isEdited: Bool
    }

    private func rendered(_ store: TimelineStore, _ conversation: ConversationID) -> [Rendered] {
        store.timeline(for: conversation).items.map {
            Rendered(id: $0.id, body: $0.body, timestamp: $0.timestamp, reactions: $0.reactions,
                     tombstone: $0.tombstone, isEdited: $0.isEdited)
        }
    }

    /// A newest-first archive page, as paging back delivers it, with a
    /// duplicate, a reaction ahead of its target, an edit, a retraction,
    /// an archived tombstone and a 1:1 row.
    private func mixedPage() -> [WireMessage] {
        var correction = roomMessage("fixed", from: "bob", stanzaID: "s6", at: date(6), source: .archive(mamID: "s6"))
        correction.replacesID = "o2"
        var retraction = roomMessage(nil, from: "carol", stanzaID: "s7", at: date(7), source: .archive(mamID: "s7"))
        retraction.retractsID = "s3"
        var tombstone = roomMessage(nil, from: "dave", stanzaID: "s4", at: date(4), source: .archive(mamID: "s4"))
        tombstone.isRetracted = true
        let bobJID = bob.with(resource: "laptop")!
        let meJID = jid("alice@waddle.test/phone")
        return [
            reaction(["👍"], to: "s1", from: "carol", at: date(8)),
            retraction,
            correction,
            roomMessage("typo", from: "bob", stanzaID: "s2", originID: "o2", at: date(2), source: .archive(mamID: "s2")),
            tombstone,
            roomMessage("gone", from: "carol", stanzaID: "s3", at: date(3), source: .archive(mamID: "s3")),
            roomMessage("first", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")),
            roomMessage("first", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")),
            directMessage("hi", from: bobJID, to: meJID, id: "d1", archiveID: "a1", at: date(5), source: .archive(mamID: "a1")),
        ]
    }

    @Test func aPageRendersTheSameAsItsMessagesOneByOne() {
        let page = mixedPage()
        let single = store()
        let singleResults = page.map { single.ingest($0, receivedAt: receivedAt) }
        let batched = store()
        let batchedResults = batched.ingest(contentsOf: page, receivedAt: receivedAt)

        #expect(rendered(batched, roomConversation).map(\.body) == ["first", "fixed", "gone", ""])
        #expect(rendered(batched, roomConversation) == rendered(single, roomConversation))
        #expect(rendered(batched, bobConversation) == rendered(single, bobConversation))
        #expect(batchedResults.map(Outcome.init) == singleResults.map(Outcome.init))
    }

    /// An older page lands on a timeline that already has newer rows. A
    /// row the page matches must be the one the one-by-one order would
    /// match, even when two loaded rows claim the same id.
    @Test func anOlderPageMergesIntoLoadedRowsAsOneByOne() {
        let bobJID = bob.with(resource: "laptop")!
        let meJID = jid("alice@waddle.test/phone")
        // Loaded, live and timestampless: answers to stanza-id s2 and origin-id o.
        let loaded = directMessage("live", from: bobJID, to: meJID, id: "o", archiveID: "s2")
        // An older archived row that only reuses `@id` o.
        var reused = directMessage("older", from: bobJID, to: meJID, id: "o", at: date(1), source: .archive(mamID: "m1"))
        reused.identity.originID = nil
        // A copy naming origin-id o: one by one, it meets the older row first.
        let copy = directMessage("copy", from: bobJID, to: meJID, id: "o", at: date(3), source: .archive(mamID: "m3"))

        let single = store()
        single.ingest(loaded, receivedAt: receivedAt)
        for message in [reused, copy] { single.ingest(message, receivedAt: receivedAt) }
        let batched = store()
        batched.ingest(loaded, receivedAt: receivedAt)
        batched.ingest(contentsOf: [reused, copy], receivedAt: receivedAt)

        #expect(rendered(single, bobConversation).map(\.timestamp) == [date(1), nil])
        #expect(rendered(batched, bobConversation) == rendered(single, bobConversation))
    }

    /// A live row inside a batch still trims the oldest rows, and reports
    /// the oldest archived row left, as on its own.
    @Test func aLiveRowInABatchTrimsAsOnItsOwn() {
        func trimmingStore() -> (TimelineStore, () -> [String?]) {
            let store = TimelineStore(maxItemsPerConversation: 3)
            store.account = me
            var cursors: [String?] = []
            store.onArchiveTrimmed = { _, cursor in cursors.append(cursor) }
            return (store, { cursors })
        }
        let page = [
            roomMessage("live", from: "bob", stanzaID: "s9"),
            roomMessage("a4", from: "bob", stanzaID: "s4", at: date(4), source: .archive(mamID: "s4")),
            roomMessage("a3", from: "bob", stanzaID: "s3", at: date(3), source: .archive(mamID: "s3")),
            roomMessage("a2", from: "bob", stanzaID: "s2", at: date(2), source: .archive(mamID: "s2")),
            roomMessage("a1", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")),
            roomMessage("live 2", from: "bob", stanzaID: "s10"),
        ]
        let (single, singleCursors) = trimmingStore()
        for message in page { single.ingest(message, receivedAt: receivedAt) }
        let (batched, batchedCursors) = trimmingStore()
        batched.ingest(contentsOf: page, receivedAt: receivedAt)

        #expect(rendered(single, roomConversation).map(\.body) == ["a4", "live", "live 2"])
        #expect(rendered(batched, roomConversation) == rendered(single, roomConversation))
        #expect(batchedCursors() == singleCursors())
        #expect(singleCursors() == ["s4"])
    }

    @Test func aPageChangesTheRenderedRowsOnce() {
        let page = (1...50).reversed().map { seq in
            roomMessage("m\(seq)", from: "bob", stanzaID: "s\(seq)", at: date(TimeInterval(seq)), source: .archive(mamID: "s\(seq)"))
        }
        let single = store()
        let singleChanges = changes(of: single.timeline(for: roomConversation)) {
            for message in page { single.ingest(message, receivedAt: receivedAt) }
        }
        let batched = store()
        let batchedChanges = changes(of: batched.timeline(for: roomConversation)) {
            batched.ingest(contentsOf: page, receivedAt: receivedAt)
        }
        #expect(singleChanges == 50)
        #expect(batchedChanges == 1)
        #expect(batched.timeline(for: roomConversation).items.count == 50)
    }

    /// How many times `work` changes the timeline's rows, as a view
    /// observing them sees it: re-tracked after each change.
    private func changes(of timeline: ConversationTimeline, during work: () -> Void) -> Int {
        let counter = ChangeCounter(timeline)
        work()
        return counter.count
    }

    @MainActor
    private final class ChangeCounter {
        private let timeline: ConversationTimeline
        private(set) var count = 0

        init(_ timeline: ConversationTimeline) {
            self.timeline = timeline
            track()
        }

        private func track() {
            withObservationTracking { _ = timeline.items } onChange: { [self] in
                MainActor.assumeIsolated {
                    count += 1
                    track()
                }
            }
        }
    }

    /// The result kind, and for an insert the row id; the inserted row
    /// itself carries a fresh `presentationID`.
    private enum Outcome: Equatable {
        case inserted(String), duplicate, mutation, ignored

        init(_ result: TimelineIngestResult) {
            switch result {
            case let .inserted(item): self = .inserted(item.id)
            case .duplicate: self = .duplicate
            case .mutation: self = .mutation
            case .ignored: self = .ignored
            }
        }
    }
}
