import Foundation
import Observation
import Testing
@testable import WaddleKit

@MainActor
@Suite("Live timeline enrichment")
struct LiveTimelineEnrichmentTests {
    private func runningSession() async -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")])
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        await eventually { coordinator.isSendReady }
        #expect(coordinator.isSendReady)
        await coordinator.open(roomConversation)
        return (coordinator, port)
    }

    @Test func lateScoresAndReactionsInvalidateTheVisibleFeed() async {
        let (coordinator, port) = await runningSession()
        let timeline = coordinator.timelines.timeline(for: roomConversation)
        port.emit(.message(roomMessage("source", from: "bob", stanzaID: "room-source", originID: "origin-source")))
        await eventually { timeline.feedItems.count == 1 }
        let rowID = timeline.feedItems.first?.id
        #expect(rowID != nil)

        let scoreObservation = FeedObservation(timeline)
        port.emit(.message(scoreMessage(originID: "origin-source")))
        await eventually { scoreObservation.invalidated && timeline.feedItems.first?.safetyScores == visibleScores }
        #expect(scoreObservation.invalidated)
        #expect(timeline.feedItems.first?.safetyScores == visibleScores)
        #expect(timeline.feedItems.map(\.id) == [rowID].compactMap { $0 })

        let reactionObservation = FeedObservation(timeline)
        port.emit(.message(reaction(["👍"], to: "room-source", from: "carol")))
        await eventually { reactionObservation.invalidated && timeline.feedItems.first?.reactions.count == 1 }
        #expect(reactionObservation.invalidated)
        #expect(timeline.feedItems.first?.reactions.first?.emoji == "👍")
        #expect(timeline.feedItems.first?.safetyScores == visibleScores)
        #expect(timeline.feedItems.map(\.id) == [rowID].compactMap { $0 })

        let clearObservation = FeedObservation(timeline)
        port.emit(.message(reaction([], to: "room-source", from: "carol")))
        await eventually { clearObservation.invalidated && timeline.feedItems.first?.reactions.isEmpty == true }
        #expect(clearObservation.invalidated)
        #expect(timeline.feedItems.first?.reactions.isEmpty == true)
        #expect(timeline.feedItems.first?.safetyScores == visibleScores)
        #expect(timeline.feedItems.map(\.id) == [rowID].compactMap { $0 })
        await coordinator.stop()
    }

    @Test(arguments: [false, true])
    func canonicalCopyEnrichesLocalEchoAndArchiveReplayPreservesIt(fromArchive: Bool) async {
        let (coordinator, port) = await runningSession()
        let timeline = coordinator.timelines.timeline(for: roomConversation)
        guard let originID = await coordinator.send(Draft(text: "source"), in: roomConversation) else {
            Issue.record("expected local send")
            await coordinator.stop()
            return
        }
        #expect(timeline.feedItems.first?.isLocalEcho == true)
        #expect(timeline.feedItems.first?.roomStanzaID == nil)

        let observation = FeedObservation(timeline)
        // Both arrive before the authoritative room copy. The event stream
        // processes these in order, parking them until its stanza ID exists.
        port.emit(.message(scoreMessage(originID: originID)))
        port.emit(.message(reaction(["👍"], to: "room-source", from: "alice")))
        let canonical = roomMessage(
            "source", from: "alice", stanzaID: "room-source", originID: originID,
            source: fromArchive ? .archive(mamID: "room-source") : .live
        )
        // A later stream event acts as a barrier before starting an archive
        // fetch, so both pending mutations really precede its canonical row.
        port.emit(.deliveryAcked(stanzaID: originID))
        await eventually { coordinator.deliveries.state(of: originID) == .acknowledged }
        #expect(coordinator.deliveries.state(of: originID) == .acknowledged)
        #expect(timeline.feedItems.first?.safetyScores == nil)
        #expect(timeline.feedItems.first?.reactions.isEmpty == true)
        #expect(!observation.invalidated)
        if fromArchive {
            port.historyPages = [ArchivePage(messages: [canonical], first: "room-source", isComplete: true)]
            await coordinator.loadLatest(roomConversation)
        } else {
            port.emit(.message(canonical))
        }
        await eventually { observation.invalidated && timeline.feedItems.first?.safetyScores == visibleScores }
        #expect(observation.invalidated)
        #expect(timeline.feedItems.count == 1)
        #expect(timeline.feedItems.first?.isLocalEcho == false)
        #expect(timeline.feedItems.first?.roomStanzaID == "room-source")
        #expect(timeline.feedItems.first?.safetyScores == visibleScores)
        #expect(timeline.feedItems.first?.reactions.first?.emoji == "👍")
        #expect(timeline.feedItems.first?.reactions.first?.includesMine == true)

        let rowID = timeline.feedItems.first?.id
        let archivedSource = roomMessage(
            "source", from: "alice", stanzaID: "room-source", originID: originID,
            at: date(1), source: .archive(mamID: "room-source")
        )
        port.historyPages = [ArchivePage(messages: [archivedSource], first: "room-source", isComplete: true)]
        await coordinator.loadLatest(roomConversation)
        #expect(timeline.feedItems.map(\.id) == [rowID].compactMap { $0 })
        #expect(timeline.feedItems.first?.safetyScores == visibleScores)
        #expect(timeline.feedItems.first?.reactions.first?.emoji == "👍")
        await coordinator.stop()
    }
}

/// Swift Observation callbacks run before the write. Read the published
/// values only after the callback has returned to the main actor.
@MainActor
private final class FeedObservation {
    private(set) var invalidated = false

    init(_ timeline: ConversationTimeline) {
        withObservationTracking {
            _ = timeline.feedItems
        } onChange: {
            Task { @MainActor in self.invalidated = true }
        }
    }
}

private let visibleScores = SafetyScores(
    modelVersion: "test-model",
    scores: [SafetyScore(category: .harassment, probability: SafetyProbability(0.9)!, taxonomyVersion: "v1")]
)

private func scoreMessage(originID: String) -> WireMessage {
    WireMessage(
        type: .groupchat,
        from: JID(bare: room, resource: nil),
        to: JID(bare: me.jid, resource: "phone"),
        identity: MessageIdentity(messageID: "room-score", originID: nil),
        safetyScores: .init(
            targetOriginID: originID, targetStanzaID: "room-source", targetStanzaBy: room,
            sourceRevisionID: "room-source", scores: visibleScores
        )
    )
}
