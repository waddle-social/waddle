import Foundation

/// A member's answer to an event. No answer is no reply.
public enum EventReply: Hashable, Sendable, CaseIterable {
    case going
    case maybe
    case declined
}

public struct EventAttendee: Hashable, Sendable {
    public let jid: BareJID
    public let reply: EventReply

    public init(jid: BareJID, reply: EventReply) {
        self.jid = jid
        self.reply = reply
    }
}

/// How an event repeats. Weekdays are UTC weekdays: the calendar has no
/// time zone, and every client walks a rule in UTC (see
/// `localWeekdays(startingAt:in:)` for the ones a person sees).
public struct EventRecurrence: Hashable, Sendable {
    public enum Frequency: Hashable, Sendable, CaseIterable {
        case daily
        case weekly
        case monthly
        case yearly
    }

    public enum End: Hashable, Sendable {
        case never
        /// This many occurrences, cancelled ones included.
        case count(Int)
        /// The last occurrence starts at or before this instant.
        case until(Date)
    }

    public let frequency: Frequency
    /// Every `interval` days, weeks, months or years.
    public let interval: Int
    /// Empty on a weekly rule means the weekday of the start.
    public let weekdays: [Weekday]
    /// 1 to 31, or -1 to -31 counted from the end of the month.
    public let monthDays: [Int]
    public let end: End

    public init(frequency: Frequency, interval: Int = 1, weekdays: [Weekday] = [], monthDays: [Int] = [], end: End = .never) {
        self.frequency = frequency
        self.interval = interval
        self.weekdays = weekdays
        self.monthDays = monthDays
        self.end = end
    }

    /// The weekdays of a series that starts at `start`, as they fall in
    /// `timeZone`, Sunday first.
    public func localWeekdays(startingAt start: CalendarMoment, in timeZone: TimeZone) -> [Weekday] {
        let offset = start.dayOffset(in: timeZone)
        let utcWeekdays = weekdays.isEmpty && frequency == .weekly
            ? [start.day(in: TimeZone(identifier: "UTC")!).weekday]
            : weekdays
        return Set(utcWeekdays.map { $0.shifted(by: offset) }).sorted()
    }
}

/// One occurrence of a repeating event that was changed on its own. A
/// missing field keeps the event's value.
public struct EventOccurrenceChange: Hashable, Sendable {
    /// The start the occurrence would have had.
    public let recurrenceStart: CalendarMoment
    public let title: String?
    public let details: String?
    public let location: String?
    public let start: CalendarMoment?
    public let end: CalendarMoment?

    public init(
        recurrenceStart: CalendarMoment,
        title: String? = nil,
        details: String? = nil,
        location: String? = nil,
        start: CalendarMoment? = nil,
        end: CalendarMoment? = nil
    ) {
        self.recurrenceStart = recurrenceStart
        self.title = title
        self.details = details
        self.location = location
        self.start = start
        self.end = end
    }
}

/// An event on the community calendar, with every member's reply.
public struct CommunityEvent: Hashable, Sendable, Identifiable {
    public let id: String
    public let uid: String
    public let title: String
    public let details: String?
    public let location: String?
    public let organizer: BareJID?
    /// The room the event belongs to, if any. Publishing writes it, so an
    /// edit hands it back.
    public let workspaceRoom: BareJID?
    /// Nil for an event not given a date yet.
    public let start: CalendarMoment?
    /// Exclusive for an all-day event; nil for a timed one is an instant.
    public let end: CalendarMoment?
    public let attendees: [EventAttendee]
    public let recurrence: EventRecurrence?
    /// Starts of the occurrences cancelled on their own.
    public let cancelled: [CalendarMoment]
    public let changed: [EventOccurrenceChange]

    public init(
        id: String,
        uid: String,
        title: String,
        details: String? = nil,
        location: String? = nil,
        organizer: BareJID? = nil,
        workspaceRoom: BareJID? = nil,
        start: CalendarMoment? = nil,
        end: CalendarMoment? = nil,
        attendees: [EventAttendee] = [],
        recurrence: EventRecurrence? = nil,
        cancelled: [CalendarMoment] = [],
        changed: [EventOccurrenceChange] = []
    ) {
        self.id = id
        self.uid = uid
        self.title = title
        self.details = details
        self.location = location
        self.organizer = organizer
        self.workspaceRoom = workspaceRoom
        self.start = start
        self.end = end
        self.attendees = attendees
        self.recurrence = recurrence
        self.cancelled = cancelled
        self.changed = changed
    }

    public func reply(of jid: BareJID) -> EventReply? {
        attendees.first { $0.jid == jid }?.reply
    }

    public func attendees(replying reply: EventReply) -> [EventAttendee] {
        attendees.filter { $0.reply == reply }
    }

    /// The event with `jid`'s reply set to `reply`, or taken out for nil.
    func replying(_ reply: EventReply?, by jid: BareJID) -> CommunityEvent {
        var attendees = attendees
        let answer = reply.map { EventAttendee(jid: jid, reply: $0) }
        if let index = attendees.firstIndex(where: { $0.jid == jid }) {
            if let answer { attendees[index] = answer } else { attendees.remove(at: index) }
        } else if let answer {
            attendees.append(answer)
        }
        return CommunityEvent(
            id: id, uid: uid, title: title, details: details, location: location,
            organizer: organizer, workspaceRoom: workspaceRoom, start: start, end: end,
            attendees: attendees, recurrence: recurrence, cancelled: cancelled, changed: changed
        )
    }
}

/// One occurrence of an event: the event itself, or one date of a series.
public struct EventOccurrence: Hashable, Sendable, Identifiable {
    public let eventID: String
    /// The start the series gives it: what it is cancelled by. Nil when the
    /// event does not repeat.
    public let recurrenceStart: CalendarMoment?
    public let start: CalendarMoment
    /// Exclusive for an all-day occurrence; nil for a timed one is an instant.
    public let end: CalendarMoment?
    public let title: String
    public let details: String?
    public let location: String?

    public init(
        eventID: String,
        recurrenceStart: CalendarMoment?,
        start: CalendarMoment,
        end: CalendarMoment?,
        title: String,
        details: String? = nil,
        location: String? = nil
    ) {
        self.eventID = eventID
        self.recurrenceStart = recurrenceStart
        self.start = start
        self.end = end
        self.title = title
        self.details = details
        self.location = location
    }

    public var id: String {
        switch recurrenceStart ?? start {
        case let .day(day): "\(eventID)/\(day.iso)"
        case let .instant(date): "\(eventID)/\(Int64((date.timeIntervalSince1970 * 1000).rounded()))"
        }
    }

    public var isAllDay: Bool { start.isAllDay }

    /// The days it runs on in `timeZone`. A timed occurrence covers each day
    /// it is on the clock (one that ends at midnight does not reach the
    /// next day); an all-day one its dates, the end being exclusive.
    public func days(in timeZone: TimeZone) -> ClosedRange<CalendarDay> {
        let first = start.day(in: timeZone)
        let last: CalendarDay
        switch (start, end) {
        case let (.day, .day(end)?):
            last = end.advanced(by: -1)
        case let (.instant(begin), .instant(end)?) where end > begin:
            last = CalendarDay(end.addingTimeInterval(-0.001), in: timeZone)
        default:
            last = first
        }
        return first...max(first, last)
    }

    /// Whether any of it falls in `range`; an instant counts at the start
    /// of the range.
    public func overlaps(_ range: Range<Date>, in timeZone: TimeZone) -> Bool {
        let begin = start.date(in: timeZone)
        let finish: Date = switch (start, end) {
        case let (.day(day), nil): day.advanced(by: 1).start(in: timeZone)
        case let (_, end?): end.date(in: timeZone)
        case (.instant, nil): begin
        }
        return begin < range.upperBound && (finish > range.lowerBound || begin >= range.lowerBound)
    }
}
