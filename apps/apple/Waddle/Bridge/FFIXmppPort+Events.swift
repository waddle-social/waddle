import Foundation
import WaddleKit

/// The community calendar: xCal events on `community.<domain>`.
extension FFIXmppPort {
    func fetchEvents() async throws -> [CommunityEvent] {
        let service = try communityService()
        let events = try await mappingPortErrors { try await client.fetchCommunityEvents(communityJid: service) }
        return events.compactMap(FFIInbound.communityEvent)
    }

    func publishEvent(_ draft: EventDraft) async throws -> CommunityEvent {
        let service = try communityService()
        let published = try await mappingPortErrors {
            try await client.publishCommunityEvent(communityJid: service, event: FFIOutbound.event(draft))
        }
        guard let event = FFIInbound.communityEvent(published) else { throw PortError.failed }
        return event
    }

    func cancelEvent(_ eventID: String) async throws {
        let service = try communityService()
        try await mappingPortErrors { try await client.retractCommunityEvent(communityJid: service, itemId: eventID) }
    }

    func cancelOccurrence(_ recurrenceStart: CalendarMoment, of eventID: String) async throws -> CommunityEvent {
        let service = try communityService()
        let series = try await mappingPortErrors {
            try await client.cancelCommunityEventOccurrence(
                communityJid: service, itemId: eventID, occurrence: FFIOutbound.calendarDate(recurrenceStart)
            )
        }
        guard let event = FFIInbound.communityEvent(series) else { throw PortError.failed }
        return event
    }

    func reply(_ reply: EventReply, toEvent uid: String) async throws {
        let service = try communityService()
        guard let account else { throw PortError.invalidRequest }
        try await mappingPortErrors {
            try await client.rsvpCommunityEvent(
                communityJid: service, uid: uid, selfJid: account.description, status: FFIOutbound.participation(reply)
            )
        }
    }

    func isCommunityOwner() async -> Bool {
        await client.isCommunityOwner()
    }

    /// All-day dates come back at midnight UTC, so the core is asked for a
    /// day more on each side; WaddleKit keeps those that fall in `range` on
    /// the person's clock.
    func occurrences(of event: CommunityEvent, in range: Range<Date>) -> [EventOccurrence] {
        let day: TimeInterval = 24 * 60 * 60
        do {
            let occurrences = try expandCommunityEvent(
                event: FFIOutbound.event(event),
                fromMs: FFIOutbound.milliseconds(range.lowerBound.addingTimeInterval(-day)),
                toMs: FFIOutbound.milliseconds(range.upperBound.addingTimeInterval(day))
            )
            return occurrences.compactMap { FFIInbound.occurrence($0, of: event.id) }
        } catch {
            BridgeLog.error("Couldn't work out the dates of event \(event.id): \(error)")
            return []
        }
    }
}

/// Community calendar records.
extension FFIOutbound {
    /// What a publish writes. The server ignores replies, cancelled and
    /// changed dates and the organizer here, and keeps its own.
    static func event(_ draft: EventDraft) -> WaddleEvent {
        WaddleEvent(
            id: draft.eventID ?? "",
            uid: "",
            summary: draft.title,
            description: draft.details,
            location: draft.location,
            organizer: nil,
            workspaceRoom: draft.workspaceRoom?.description,
            start: draft.start.map(calendarDate),
            end: draft.end.map(calendarDate),
            attendees: [],
            recurrence: draft.recurrence.map(recurrence),
            excluded: [],
            overrides: []
        )
    }

    /// The stored shape of a fetched event, for working out its dates.
    static func event(_ event: CommunityEvent) -> WaddleEvent {
        WaddleEvent(
            id: event.id,
            uid: event.uid,
            summary: event.title,
            description: event.details,
            location: event.location,
            organizer: nil,
            workspaceRoom: event.workspaceRoom?.description,
            start: event.start.map(calendarDate),
            end: event.end.map(calendarDate),
            attendees: [],
            recurrence: event.recurrence.map(recurrence),
            excluded: event.cancelled.map(calendarDate),
            overrides: event.changed.map { change in
                WaddleEventOverride(
                    recurrenceId: calendarDate(change.recurrenceStart),
                    summary: change.title,
                    description: change.details,
                    location: change.location,
                    start: change.start.map(calendarDate),
                    end: change.end.map(calendarDate)
                )
            }
        )
    }

    static func calendarDate(_ moment: CalendarMoment) -> WaddleCalendarDate {
        switch moment {
        case let .day(day): .date(date: day.iso)
        case let .instant(date): .dateTime(timestampMs: milliseconds(date))
        }
    }

    static func milliseconds(_ date: Date) -> Int64 {
        Int64((date.timeIntervalSince1970 * 1000).rounded())
    }

    static func participation(_ reply: EventReply) -> WaddleEventParticipation {
        switch reply {
        case .going: .accepted
        case .maybe: .tentative
        case .declined: .declined
        }
    }

    private static func recurrence(_ rule: EventRecurrence) -> WaddleRecurrence {
        let end: WaddleRecurrenceEnd = switch rule.end {
        case .never: .never
        case let .count(count): .count(count: UInt32(clamping: count))
        case let .until(date): .until(timestampMs: milliseconds(date))
        }
        return WaddleRecurrence(
            frequency: frequency(rule.frequency),
            interval: UInt32(clamping: rule.interval),
            weekdays: rule.weekdays.map(weekday),
            monthDays: rule.monthDays.map { Int32(clamping: $0) },
            end: end
        )
    }

    private static func frequency(_ frequency: EventRecurrence.Frequency) -> WaddleRecurrenceFrequency {
        switch frequency {
        case .daily: .daily
        case .weekly: .weekly
        case .monthly: .monthly
        case .yearly: .yearly
        }
    }

    private static func weekday(_ weekday: Weekday) -> WaddleWeekday {
        switch weekday {
        case .sunday: .sunday
        case .monday: .monday
        case .tuesday: .tuesday
        case .wednesday: .wednesday
        case .thursday: .thursday
        case .friday: .friday
        case .saturday: .saturday
        }
    }
}
