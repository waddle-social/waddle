import Foundation

extension SessionCoordinator {
    /// Reloads the community's events and whether this account may manage
    /// them. A failed fetch keeps what was shown.
    public func refreshEvents(at now: Date = Date()) async {
        let refresh = events.begin()
        guard !isStopped, connection == .online else {
            events.cancel(refresh)
            return
        }
        let port = self.port
        async let fetched = Self.attempt { try await port.fetchEvents() }
        async let isOwner = port.isCommunityOwner()
        let (list, canManage) = await (fetched, isOwner)
        guard events.isCurrent(refresh) else { return }
        if let list { events.publish(events: list, refresh: refresh) }
        // The same question the admin console's entry asks once connected.
        if canManage { isCommunityOwner = true }
        events.finish(failed: list == nil, canManage: canManage, at: now)
    }

    /// Creates or edits an event and shows it as the server stored it.
    /// Throws `PortError`.
    @discardableResult
    public func saveEvent(_ draft: EventDraft) async throws -> CommunityEvent {
        let event = try await whileOnline { try await port.publishEvent(draft) }
        events.save(event)
        return event
    }

    /// Cancels an event, every date of it. Throws `PortError`.
    public func cancelEvent(_ eventID: String) async throws {
        try await whileOnline { try await port.cancelEvent(eventID) }
        events.remove(eventID)
    }

    /// Cancels one date of a series and keeps the rest. Throws `PortError`;
    /// `invalidRequest` for an event that does not repeat.
    public func cancelOccurrence(_ occurrence: EventOccurrence) async throws {
        guard let recurrenceStart = occurrence.recurrenceStart else { throw PortError.invalidRequest }
        let event = try await whileOnline {
            try await port.cancelOccurrence(recurrenceStart, of: occurrence.eventID)
        }
        events.save(event)
    }

    /// Sets the user's reply to an event. Shown at once and undone if the
    /// server refuses; replies to one event are sent one after another, so
    /// quick taps end as the last one left them. Choosing the reply already
    /// shown asks nothing. Throws `PortError`.
    public func reply(_ reply: EventReply, to eventID: String) async throws {
        guard !isStopped, connection == .online else { throw PortError.notConnected }
        guard let event = events.event(eventID) else { throw PortError.invalidRequest }
        guard events.myReply(to: eventID) != reply else { return }
        let edit = events.edit(reply, of: eventID)
        let port = self.port
        let uid = event.uid
        let write = events.afterEarlierReplyWrites(of: eventID) {
            try await port.reply(reply, toEvent: uid)
        }
        do {
            try await write.value
            events.sent(reply, of: eventID)
        } catch {
            events.undo(edit, of: eventID)
            throw error
        }
    }

    /// Every date of every event that falls in `range` as seen in
    /// `timeZone`, earliest first, all-day ones first on their day.
    public func eventOccurrences(in range: Range<Date>, timeZone: TimeZone) -> [EventOccurrence] {
        events.events
            .filter { $0.start != nil }
            .flatMap { port.occurrences(of: $0, in: range) }
            .filter { $0.overlaps(range, in: timeZone) }
            .map { (occurrence: $0, start: $0.start.date(in: timeZone)) }
            .sorted { first, second in
                if first.start != second.start { return first.start < second.start }
                if first.occurrence.isAllDay != second.occurrence.isAllDay { return first.occurrence.isAllDay }
                return (first.occurrence.title, first.occurrence.id) < (second.occurrence.title, second.occurrence.id)
            }
            .map(\.occurrence)
    }
}
