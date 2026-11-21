import Foundation

/// What the event editor holds: the person's picks, in their time zone.
/// `draft()` turns them into what the calendar stores, which has no time
/// zone: all-day dates, UTC instants, and a rule walked in UTC.
public struct EventForm: Equatable, Sendable {
    public enum Repeat: Hashable, Sendable, CaseIterable {
        case never, daily, weekly, monthly, yearly
    }

    public enum Ending: Hashable, Sendable, CaseIterable {
        case never
        /// After `count` times.
        case after
        /// On the day `until` falls on.
        case on
    }

    public var title = ""
    /// False for an event not given a date yet; it keeps none.
    public var hasDate = true
    public var isAllDay = false {
        didSet {
            guard isAllDay else {
                // A last day is not an end time: give the event an hour.
                if oldValue, end <= start { end = start.addingTimeInterval(3600) }
                return
            }
            // An all-day series cannot end on a date, and an all-day event
            // always has a last day.
            if ending == .on { ending = .never }
            if !hasEnd {
                end = start
                hasEnd = true
            }
        }
    }

    /// The start time; for an all-day event, any time on the first day.
    public var start: Date
    /// The end time; for an all-day event, any time on the last day.
    public var end: Date
    /// A timed event without an end is an instant.
    public var hasEnd = true
    public var repeats: Repeat = .never
    public var interval = 1
    /// The weekdays of a weekly series, as the person sees them. The start's
    /// weekday is always one of them (`shownWeekdays()`).
    public var weekdays: Set<Weekday> = []
    public var ending: Ending = .never
    public var count = 10
    public var until: Date
    public var location = ""
    public var details = ""

    /// Nil for a new event.
    public private(set) var eventID: String?
    public let timeZone: TimeZone
    private var workspaceRoom: BareJID?
    private var stored: Stored?

    /// What was fetched, so an untouched rule goes back as it came.
    private struct Stored: Equatable, Sendable {
        let start: CalendarMoment?
        let recurrence: EventRecurrence?
        let picks: RepeatPicks
        let hasCancelledOrChangedDates: Bool
    }

    private struct RepeatPicks: Equatable, Sendable {
        let repeats: Repeat
        let interval: Int
        let weekdays: Set<Weekday>
        let ending: Ending
        let count: Int
        let untilDay: CalendarDay
    }

    /// A new event on `day`: at the next full hour when that is today,
    /// otherwise at ten, for an hour.
    public init(on day: CalendarDay, now: Date, timeZone: TimeZone) {
        let calendar = CalendarDay.gregorian(timeZone)
        let start = day == CalendarDay(now, in: timeZone)
            ? calendar.dateInterval(of: .hour, for: now)!.end
            : calendar.date(bySettingHour: 10, minute: 0, second: 0, of: day.start(in: timeZone))!
        self.start = start
        end = start.addingTimeInterval(3600)
        until = Self.aMonthAfter(start, in: timeZone)
        eventID = nil
        self.timeZone = timeZone
        workspaceRoom = nil
        stored = nil
    }

    /// The picks that show `event` as it is.
    public init(editing event: CommunityEvent, now: Date = Date(), timeZone: TimeZone) {
        self.init(on: CalendarDay(now, in: timeZone), now: now, timeZone: timeZone)
        title = event.title
        details = event.details ?? ""
        location = event.location ?? ""
        switch event.start {
        case nil:
            hasDate = false
        case let .day(first)?:
            isAllDay = true
            start = first.start(in: timeZone)
            if case let .day(after)? = event.end, after > first {
                end = after.advanced(by: -1).start(in: timeZone)
            } else {
                end = start
            }
        case let .instant(date)?:
            start = date
            if case let .instant(finish)? = event.end {
                end = finish
            } else {
                end = date.addingTimeInterval(3600)
                hasEnd = false
            }
        }
        until = Self.aMonthAfter(start, in: timeZone)
        if let rule = event.recurrence, let first = event.start {
            repeats = Repeat(rule.frequency)
            interval = rule.interval
            if rule.frequency == .weekly {
                weekdays = Set(rule.localWeekdays(startingAt: first, in: timeZone))
            }
            switch rule.end {
            case .never: ending = .never
            case let .count(times):
                ending = .after
                count = times
            case let .until(last):
                ending = .on
                until = last
            }
        }
        eventID = event.id
        workspaceRoom = event.workspaceRoom
        stored = Stored(
            start: event.start,
            recurrence: event.recurrence,
            picks: picks,
            hasCancelledOrChangedDates: !event.cancelled.isEmpty || !event.changed.isEmpty
        )
    }

    /// An all-day series can only end never or after some times.
    public var offersEndDate: Bool { !isAllDay }

    /// Where "ends on a date" begins: never before the start.
    private static func aMonthAfter(_ start: Date, in timeZone: TimeZone) -> Date {
        CalendarDay.gregorian(timeZone).date(byAdding: .month, value: 1, to: start)!
    }

    /// Moves the start and keeps the length: the same time, or for an
    /// all-day event the same number of days. A weekly series moves to the
    /// new weekday, and a last date left behind moves past the start.
    public mutating func moveStart(to date: Date) {
        let weekday = CalendarDay(start, in: timeZone).weekday
        if CalendarDay(date, in: timeZone).weekday != weekday { weekdays.remove(weekday) }
        if until < date { until = Self.aMonthAfter(date, in: timeZone) }
        if isAllDay {
            let days = CalendarDay(start, in: timeZone).distance(to: CalendarDay(end, in: timeZone))
            end = CalendarDay(date, in: timeZone).advanced(by: max(days, 0)).start(in: timeZone)
        } else {
            end = date.addingTimeInterval(max(end.timeIntervalSince(start), 0))
        }
        start = date
    }

    /// The weekdays a weekly series is shown with: those picked, and the
    /// start's.
    public func shownWeekdays() -> Set<Weekday> {
        weekdays.union([CalendarDay(start, in: timeZone).weekday])
    }

    /// Saving moves the start of a series that has dates cancelled or
    /// changed on their own; those come back as the series has them.
    public func bringsBackCancelledDates() -> Bool {
        guard let stored, stored.hasCancelledOrChangedDates, hasDate, repeats != .never else { return false }
        return stored.start != startMoment
    }

    /// What to save, or nil while the server would refuse it.
    public func draft() -> EventDraft? {
        guard hasDate else {
            return EventDraft(
                eventID: eventID, title: title, details: details, location: location, workspaceRoom: workspaceRoom
            )
        }
        let endMoment: CalendarMoment?
        if isAllDay {
            let first = CalendarDay(start, in: timeZone)
            let last = CalendarDay(end, in: timeZone)
            guard last >= first else { return nil }
            endMoment = .day(last.advanced(by: 1))
        } else {
            endMoment = hasEnd ? .instant(end) : nil
        }
        return EventDraft(
            eventID: eventID,
            title: title,
            details: details,
            location: location,
            start: startMoment,
            end: endMoment,
            recurrence: recurrence,
            workspaceRoom: workspaceRoom,
            storedRecurrence: stored?.recurrence
        )
    }

    private var startMoment: CalendarMoment {
        isAllDay ? .day(CalendarDay(start, in: timeZone)) : .instant(start)
    }

    private var picks: RepeatPicks {
        RepeatPicks(
            repeats: repeats,
            interval: interval,
            weekdays: weekdays,
            ending: ending,
            count: count,
            untilDay: CalendarDay(until, in: timeZone)
        )
    }

    private var recurrence: EventRecurrence? {
        guard let frequency = repeats.frequency else { return nil }
        if let stored, let rule = stored.recurrence, stored.picks == picks, stored.start == startMoment {
            return rule
        }
        let offset = startMoment.dayOffset(in: timeZone)
        let utcWeekdays = frequency == .weekly ? shownWeekdays().map { $0.shifted(by: -offset) }.sorted() : []
        // Month days come only from a rule another client wrote; keep them
        // while the series stays monthly.
        let monthDays = frequency == .monthly && stored?.recurrence?.frequency == .monthly
            ? stored?.recurrence?.monthDays ?? []
            : []
        let end: EventRecurrence.End = switch ending {
        case .never: .never
        case .after: .count(count)
        case .on: .until(CalendarDay(until, in: timeZone).advanced(by: 1).start(in: timeZone).addingTimeInterval(-1))
        }
        return EventRecurrence(frequency: frequency, interval: interval, weekdays: utcWeekdays, monthDays: monthDays, end: end)
    }
}

extension EventForm.Repeat {
    init(_ frequency: EventRecurrence.Frequency) {
        switch frequency {
        case .daily: self = .daily
        case .weekly: self = .weekly
        case .monthly: self = .monthly
        case .yearly: self = .yearly
        }
    }

    var frequency: EventRecurrence.Frequency? {
        switch self {
        case .never: nil
        case .daily: .daily
        case .weekly: .weekly
        case .monthly: .monthly
        case .yearly: .yearly
        }
    }
}
