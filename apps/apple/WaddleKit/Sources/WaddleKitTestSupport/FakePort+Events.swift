import Foundation
import WaddleKit

/// The scripted community calendar of a `FakePort`.
@MainActor
final class FakeEvents {
    var list: Result<[CommunityEvent], PortError> = .success([])
    var isOwner = false
    var publishResult: Result<Void, PortError> = .success(())
    var cancelResult: Result<Void, PortError> = .success(())
    var replyResult: Result<Void, PortError> = .success(())
    /// The dates `occurrences(of:in:)` gives each event, by event id. The
    /// fake cannot walk a rule: an event left out has one date, its start,
    /// unless it repeats.
    var occurrences: [String: [EventOccurrence]] = [:]

    var fetchRequests = 0
    var ownerRequests = 0
    var published: [EventDraft] = []
    var retracted: [String] = []
    var cancelledOccurrences: [(event: String, start: CalendarMoment)] = []
    var replies: [(uid: String, reply: EventReply)] = []

    /// Verbs that wait for `release()` before answering.
    var held: Set<String> = []
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func hold(_ verb: String) async {
        if held.contains(verb) {
            await withCheckedContinuation { waiting.append($0) }
        }
    }

    func release() {
        let waiting = waiting
        self.waiting = []
        waiting.forEach { $0.resume() }
    }
}

extension FakePort {
    func fetchEvents() async throws -> [CommunityEvent] {
        calendar.fetchRequests += 1
        // The answer is what the server held when asked.
        let answer = calendar.list
        await calendar.hold("fetchEvents")
        return try answer.get()
    }

    func publishEvent(_ draft: EventDraft) async throws -> CommunityEvent {
        calendar.published.append(draft)
        await calendar.hold("publishEvent")
        try calendar.publishResult.get()
        let id = draft.eventID ?? "event-\(calendar.published.count)"
        return CommunityEvent(
            id: id,
            uid: id,
            title: draft.title,
            details: draft.details,
            location: draft.location,
            organizer: me.jid,
            workspaceRoom: draft.workspaceRoom,
            start: draft.start,
            end: draft.end,
            recurrence: draft.recurrence
        )
    }

    func cancelEvent(_ eventID: String) async throws {
        calendar.retracted.append(eventID)
        try calendar.cancelResult.get()
    }

    func cancelOccurrence(_ recurrenceStart: CalendarMoment, of eventID: String) async throws -> CommunityEvent {
        calendar.cancelledOccurrences.append((eventID, recurrenceStart))
        try calendar.cancelResult.get()
        guard let event = try calendar.list.get().first(where: { $0.id == eventID }) else {
            throw PortError.invalidRequest
        }
        return CommunityEvent(
            id: event.id, uid: event.uid, title: event.title, details: event.details, location: event.location,
            organizer: event.organizer, workspaceRoom: event.workspaceRoom, start: event.start, end: event.end,
            attendees: event.attendees, recurrence: event.recurrence,
            cancelled: event.cancelled + [recurrenceStart], changed: event.changed
        )
    }

    func reply(_ reply: EventReply, toEvent uid: String) async throws {
        calendar.replies.append((uid, reply))
        await calendar.hold("reply")
        try calendar.replyResult.get()
    }

    func isCommunityOwner() async -> Bool {
        calendar.ownerRequests += 1
        let answer = calendar.isOwner
        await calendar.hold("isCommunityOwner")
        return answer
    }

    func occurrences(of event: CommunityEvent, in _: Range<Date>) -> [EventOccurrence] {
        if let scripted = calendar.occurrences[event.id] { return scripted }
        guard let start = event.start, event.recurrence == nil else { return [] }
        return [EventOccurrence(
            eventID: event.id, recurrenceStart: nil, start: start, end: event.end,
            title: event.title, details: event.details, location: event.location
        )]
    }
}
