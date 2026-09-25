import Foundation
import Observation
import WaddleKit

/// Message actions for one timeline (a conversation or a thread): what
/// rows trigger and the confirmations the screen presents.
@MainActor
@Observable
final class MessageActionModel {
    let conversation: ConversationID
    let composer: ComposerModel
    /// Rows inside a thread do not offer "Reply in thread".
    let isThread: Bool

    var pendingDeletion: TimelineItem?
    var pendingRemoval: TimelineItem?
    var removalReason = ""
    /// The row whose full emoji picker is open.
    var reactionTarget: TimelineItem?
    /// The row whose safety-score breakdown is open.
    var safetyScoresTarget: TimelineItem?
    var errorMessage: String?
    /// A row the timeline should scroll to, then clear.
    var scrollRequest: String?
    /// A row briefly highlighted after scrolling to it.
    private(set) var highlightedID: String?
    /// Incremented to ask the composer to take focus.
    private(set) var focusRequest = 0

    @ObservationIgnored private var highlightTask: Task<Void, Never>?

    init(conversation: ConversationID, composer: ComposerModel, isThread: Bool) {
        self.conversation = conversation
        self.composer = composer
        self.isThread = isThread
    }

    func reply(to item: TimelineItem) {
        composer.beginReply(to: item)
        focusRequest += 1
    }

    func edit(_ item: TimelineItem) {
        composer.beginEdit(item)
        focusRequest += 1
    }

    func pickReaction(for item: TimelineItem) {
        reactionTarget = item
    }

    func showSafetyScores(for item: TimelineItem) {
        safetyScoresTarget = item
    }

    func requestDeletion(of item: TimelineItem) {
        pendingDeletion = item
    }

    func requestRemoval(of item: TimelineItem) {
        removalReason = ""
        pendingRemoval = item
    }

    /// XEP-0444 sends our full set, so toggle against the freshest row, not
    /// a snapshot a picker may have held while other reactions arrived.
    func react(_ emoji: String, to item: TimelineItem, session: SessionCoordinator) {
        let current = session.timelines.timeline(for: item.conversation).item(withID: item.id) ?? item
        Task {
            let succeeded = await session.toggleReaction(emoji, on: current)
            if !succeeded {
                errorMessage = "Couldn't update the reaction. Try again."
            }
        }
    }

    func setPinned(_ pinned: Bool, _ item: TimelineItem, session: SessionCoordinator) {
        Task {
            let succeeded = await session.setPinned(pinned, item)
            if !succeeded {
                errorMessage = pinned ? "Couldn't pin the message." : "Couldn't unpin the message."
            }
        }
    }

    /// XEP-0424 retraction of an own message.
    func delete(_ item: TimelineItem, session: SessionCoordinator) async {
        pendingDeletion = nil
        let succeeded = await session.retract(item)
        if !succeeded {
            errorMessage = "Couldn't delete the message. Try again."
        }
    }

    /// XEP-0425 moderation of someone else's message.
    func remove(_ item: TimelineItem, reason: String, session: SessionCoordinator) async {
        pendingRemoval = nil
        let trimmed = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        let succeeded = await session.moderate(item, reason: trimmed.isEmpty ? nil : trimmed)
        if !succeeded {
            errorMessage = "Couldn't remove the message. Try again."
        }
    }

    func showMessage(_ id: String) {
        scrollRequest = id
    }

    func highlight(_ id: String) {
        highlightedID = id
        highlightTask?.cancel()
        highlightTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_600_000_000)
            guard !Task.isCancelled else { return }
            self?.highlightedID = nil
        }
    }
}
