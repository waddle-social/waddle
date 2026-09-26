import Foundation
import Testing
@testable import WaddleKit

/// A room-authored XEP-0422 fastening carrying `urn:waddle:safety-scores:1`,
/// as the Rust core hands it over after parsing.
private func scoresFastening(
    to target: String,
    _ scores: SafetyScores,
    originID: String = "origin-1",
    revisionID: String? = nil,
    from sender: JID = jid("general@muc.waddle.test"),
    type: MessageType = .groupchat,
    at timestamp: Date? = nil
) -> WireMessage {
    WireMessage(
        type: type,
        from: sender,
        to: JID(bare: me.jid, resource: "phone"),
        identity: MessageIdentity(messageID: UUID().uuidString, originID: nil),
        timestamp: timestamp,
        safetyScores: WireMessage.SafetyScoresFastening(
            targetOriginID: originID,
            targetStanzaID: target,
            targetStanzaBy: room,
            sourceRevisionID: revisionID ?? target,
            scores: scores
        )
    )
}

private func scoredRoomMessage(
    _ body: String?, from nick: String, stanzaID: String,
    originID: String = "origin-1",
    at timestamp: Date? = nil, source: WireMessage.Source = .live
) -> WireMessage {
    roomMessage(body, from: nick, stanzaID: stanzaID, originID: originID, at: timestamp, source: source)
}

private func score(_ category: SafetyCategory, _ value: Double, _ taxonomy: String = "v1") -> SafetyScore {
    SafetyScore(category: category, probability: SafetyProbability(value)!, taxonomyVersion: taxonomy)
}

private let firstBatch = SafetyScores(
    modelVersion: "typesafe/jev-1.13-20260917",
    scores: [
        score(.isQuestion, 0.92, "is-question-v1"),
        score(.hateSpeech, 0.03, "safety-hate-speech-v1"),
        score(.explicit, 0.01, "safety-explicit-v1"),
        score(.harassment, 0.02, "safety-harassment-v1"),
        score(.violence, 0.0, "safety-violence-v1"),
        score(.selfHarm, 0.0, "safety-self-harm-v1"),
    ]
)

private let secondBatch = SafetyScores(
    modelVersion: "typesafe/jev-1.14-20261001",
    scores: [score(.violence, 0.8, "safety-violence-v2")]
)

@MainActor
@Suite("Safety scores (XEP-0422 fastening)")
struct SafetyScoresTests {
    private func store() -> TimelineStore {
        let store = TimelineStore()
        store.account = me
        return store
    }

    private func row(_ store: TimelineStore, _ conversation: ConversationID = roomConversation) -> TimelineItem? {
        store.timeline(for: conversation).items.first
    }

    @Test func roomScoresAttachToTheRowTheyTarget() {
        let store = store()
        store.ingest(scoredRoomMessage("is this on?", from: "bob", stanzaID: "s1"))
        let result = store.ingest(scoresFastening(to: "s1", firstBatch))
        #expect(result == .mutation)
        #expect(store.timeline(for: roomConversation).items.count == 1)
        #expect(row(store)?.safetyScores == firstBatch)
        #expect(row(store)?.body == "is this on?")
    }

    @Test func aNewerFasteningReplacesTheScores() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1"))
        store.ingest(scoresFastening(to: "s1", firstBatch))
        store.ingest(scoresFastening(to: "s1", secondBatch))
        #expect(row(store)?.safetyScores == secondBatch)
    }

    @Test func aCorrectionClearsOldScoresAndRejectsOldRevisionReplay() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1"))
        store.ingest(scoresFastening(to: "s1", firstBatch))
        var correction = scoredRoomMessage("edited", from: "bob", stanzaID: "edit-1")
        correction.replacesID = "origin-1"
        store.ingest(correction)
        #expect(row(store)?.safetyScores == nil)
        store.ingest(scoresFastening(to: "s1", firstBatch))
        #expect(row(store)?.safetyScores == nil)
        store.ingest(scoresFastening(to: "s1", secondBatch, revisionID: "edit-1"))
        #expect(row(store)?.safetyScores == secondBatch)
    }

    @Test func anOlderArchivedFasteningDoesNotOverrideANewerOne() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")))
        store.ingest(scoresFastening(to: "s1", secondBatch, at: date(10)))
        store.ingest(scoresFastening(to: "s1", firstBatch, at: date(5)))
        #expect(row(store)?.safetyScores == secondBatch)
    }

    @Test func aFasteningBeforeItsTargetIsParkedThenApplied() {
        // Backwards MAM paging loads the fastening before the message.
        let store = store()
        store.ingest(scoresFastening(to: "s1", firstBatch, at: date(10)))
        #expect(store.timeline(for: roomConversation).items.isEmpty)
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")))
        #expect(row(store)?.safetyScores == firstBatch)
    }

    @Test func newestFirstArchiveKeepsScoreUntilItsCorrectionArrives() {
        let store = store()
        store.ingest(scoresFastening(to: "s1", secondBatch, revisionID: "edit-1", at: date(10)))
        var correction = scoredRoomMessage("edited", from: "bob", stanzaID: "edit-1", at: date(5), source: .archive(mamID: "edit-1"))
        correction.replacesID = "origin-1"
        store.ingest(correction)
        store.ingest(scoredRoomMessage("original", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")))
        #expect(row(store)?.body == "edited")
        #expect(row(store)?.safetyScores == secondBatch)
    }

    @Test func anOccupantCannotSetScores() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1"))
        store.ingest(scoresFastening(to: "s1", firstBatch, from: jid("general@muc.waddle.test/eve")))
        #expect(row(store)?.safetyScores == nil)
    }

    @Test func anotherRoomCannotSetScores() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1"))
        // Routed to its own room, so it never reaches this row.
        store.ingest(scoresFastening(to: "s1", firstBatch, from: jid("other@muc.waddle.test")))
        #expect(row(store)?.safetyScores == nil)
    }

    @Test func originAndRoomStanzaMustMatchTheSameSource() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "room-1"))
        store.ingest(scoresFastening(to: "room-1", firstBatch, originID: "wrong-origin"))
        #expect(row(store)?.safetyScores == nil)
        store.ingest(scoresFastening(to: "room-1", firstBatch))
        #expect(row(store)?.safetyScores == firstBatch)
    }

    @Test func directMessageFasteningsAreIgnored() {
        let store = store()
        store.ingest(directMessage("hi", from: jid("bob@waddle.test/laptop"), to: jid("alice@waddle.test"), id: "m1"))
        let result = store.ingest(scoresFastening(to: "m1", firstBatch, from: jid("bob@waddle.test/laptop"), type: .chat))
        #expect(result == .ignored)
        #expect(row(store, bobConversation)?.safetyScores == nil)
        #expect(store.timeline(for: bobConversation).items.count == 1)
    }

    @Test func aRemovedMessageShowsNoScores() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1"))
        store.ingest(scoresFastening(to: "s1", firstBatch))
        var moderation = WireMessage(
            type: .groupchat,
            from: JID(bare: room, resource: nil),
            to: JID(bare: me.jid, resource: "phone"),
            identity: MessageIdentity(messageID: "mod-1", originID: nil)
        )
        moderation.moderation = .init(targetID: "s1", moderatedBy: nil, reason: nil)
        store.ingest(moderation)
        #expect(row(store)?.tombstone != nil)
        #expect(row(store)?.safetyScores == nil)
    }

    @Test func scoresSurviveTheArchiveCopyOfTheirRow() {
        let store = store()
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1", source: .archive(mamID: "s1")))
        store.ingest(scoresFastening(to: "s1", firstBatch))
        // The live copy replaces its archived twin; mutations are kept.
        store.ingest(scoredRoomMessage("msg", from: "bob", stanzaID: "s1"))
        #expect(row(store)?.safetyScores == firstBatch)
    }

    @Test func aFasteningIsAMutationNotContent() {
        let fastening = scoresFastening(to: "s1", firstBatch)
        #expect(fastening.isMutation)
        guard case let .safetyScores(target, _, parsed) = MessageMutation.of(fastening, isMine: false) else {
            Issue.record("expected a safety-scores mutation")
            return
        }
        #expect(target == "s1")
        #expect(parsed.scores == firstBatch)
    }

    @Test func aFasteningNeitherCountsUnreadNorAlerts() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        var alerts: [IncomingAlert] = []
        coordinator.onAlert = { alerts.append($0) }
        coordinator.route(scoredRoomMessage("msg", from: "bob", stanzaID: "s1"))
        coordinator.route(scoresFastening(to: "s1", firstBatch))
        #expect(coordinator.unread.count(for: roomConversation) == 1)
        #expect(alerts.isEmpty)
        #expect(coordinator.timelines.timeline(for: roomConversation).items.first?.safetyScores == firstBatch)
    }
}

@Suite("Safety scores presentation")
struct SafetyScoresPresentationTests {
    @Test func probabilityRejectsValuesOutsideTheUnitInterval() {
        #expect(SafetyProbability(0) != nil)
        #expect(SafetyProbability(1) != nil)
        #expect(SafetyProbability(-0.01) == nil)
        #expect(SafetyProbability(1.01) == nil)
        #expect(SafetyProbability(.nan) == nil)
        #expect(SafetyProbability(.infinity) == nil)
    }

    @Test func categoryRawValuesAreTheServerJudgmentNames() {
        #expect(SafetyCategory.allCases.map(\.rawValue) == [
            "is_question",
            "safety:hate_speech",
            "safety:explicit",
            "safety:harassment",
            "safety:violence",
            "safety:self_harm",
        ])
    }

    @Test func aRepeatedCategoryKeepsItsFirstScore() {
        let scores = SafetyScores(modelVersion: "m1", scores: [score(.violence, 0.1), score(.violence, 0.9)])
        #expect(scores.scores.count == 1)
        #expect(scores.score(for: .violence)?.probability.value == 0.1)
    }

    @Test func rowsFollowCategoryOrderNotWireOrder() {
        let scores = SafetyScores(modelVersion: "m1", scores: [score(.selfHarm, 0.2), score(.isQuestion, 0.5), score(.hateSpeech, 0.1)])
        #expect(scores.rows.map(\.category) == [.isQuestion, .hateSpeech, .selfHarm])
        #expect(scores.signalRows.map(\.category) == [.isQuestion])
        #expect(scores.safetyRows.map(\.category) == [.hateSpeech, .selfHarm])
    }

    @Test func percentTextRoundsAndKeepsTinyScoresVisible() {
        let scores = SafetyScores(modelVersion: "m1", scores: [
            score(.isQuestion, 0.92),
            score(.hateSpeech, 0.004),
            score(.explicit, 0.0),
            score(.violence, 1.0),
        ])
        #expect(scores.rows.map(\.percentText) == ["92%", "<1%", "0%", "100%"])
        #expect(scores.spokenSummary == "Question 92%, Hate speech <1%, Sexually explicit 0%, Violence 100%")
    }
}
