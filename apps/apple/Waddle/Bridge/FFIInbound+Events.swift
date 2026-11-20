import Foundation
import WaddleKit

/// Community calendar conversions.
extension FFIInbound {
    /// An event whose start, end or rule does not parse is dropped; so is
    /// a cancelled or changed date that does not.
    static func communityEvent(_ event: WaddleEvent) -> CommunityEvent? {
        guard let start = optionalMoment(event.start),
              let end = optionalMoment(event.end),
              let recurrence = optional(event.recurrence, Self.recurrence)
        else { return nil }
        return CommunityEvent(
            id: event.id,
            uid: event.uid,
            title: event.summary,
            details: event.description,
            location: event.location,
            organizer: xmppAccount(event.organizer),
            workspaceRoom: bareJID(event.workspaceRoom),
            start: start,
            end: end,
            attendees: event.attendees.compactMap(attendee),
            recurrence: recurrence,
            cancelled: event.excluded.compactMap(moment),
            changed: event.overrides.compactMap(occurrenceChange)
        )
    }

    static func occurrence(_ occurrence: WaddleEventOccurrence, of eventID: String) -> EventOccurrence? {
        guard let start = moment(occurrence.start),
              let recurrenceStart = optionalMoment(occurrence.recurrenceId),
              let end = optionalMoment(occurrence.end)
        else { return nil }
        return EventOccurrence(
            eventID: eventID,
            recurrenceStart: recurrenceStart,
            start: start,
            end: end,
            title: occurrence.summary,
            details: occurrence.description,
            location: occurrence.location
        )
    }

    static func moment(_ value: WaddleCalendarDate) -> CalendarMoment? {
        switch value {
        case let .date(date): CalendarDay(iso: date).map(CalendarMoment.day)
        case let .dateTime(milliseconds): .instant(self.date(milliseconds: milliseconds))
        }
    }

    /// `.some(nil)` when there is no value; nil when there is one that does
    /// not parse.
    private static func optionalMoment(_ value: WaddleCalendarDate?) -> CalendarMoment?? {
        optional(value, moment)
    }

    private static func optional<Raw, Value>(_ raw: Raw?, _ parse: (Raw) -> Value?) -> Value?? {
        guard let raw else { return .some(nil) }
        return parse(raw).map { .some($0) }
    }

    /// An attendee who has not answered is no reply; one whose address is
    /// not an `xmpp:` account is dropped.
    private static func attendee(_ attendee: WaddleEventAttendee) -> EventAttendee? {
        guard let jid = xmppAccount(attendee.uri) else { return nil }
        let reply: EventReply? = switch attendee.status {
        case .accepted: .going
        case .tentative: .maybe
        case .declined: .declined
        case .needsAction: nil
        }
        return reply.map { EventAttendee(jid: jid, reply: $0) }
    }

    /// The bare JID of an `xmpp:` URI (RFC 5122) naming an account.
    static func xmppAccount(_ uri: String?) -> BareJID? {
        guard let uri, uri.lowercased().hasPrefix("xmpp:") else { return nil }
        let path = uri.dropFirst("xmpp:".count).split(separator: "?", maxSplits: 1).first.map(String.init) ?? ""
        return bareJID(path.removingPercentEncoding)
    }

    private static func recurrence(_ rule: WaddleRecurrence) -> EventRecurrence? {
        let end: EventRecurrence.End = switch rule.end {
        case .never: .never
        case let .count(count): .count(Int(count))
        case let .until(milliseconds): .until(date(milliseconds: milliseconds))
        }
        return EventRecurrence(
            frequency: frequency(rule.frequency),
            interval: Int(rule.interval),
            weekdays: rule.weekdays.map(weekday),
            monthDays: rule.monthDays.map(Int.init),
            end: end
        )
    }

    private static func frequency(_ frequency: WaddleRecurrenceFrequency) -> EventRecurrence.Frequency {
        switch frequency {
        case .daily: .daily
        case .weekly: .weekly
        case .monthly: .monthly
        case .yearly: .yearly
        }
    }

    private static func weekday(_ weekday: WaddleWeekday) -> Weekday {
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

    private static func occurrenceChange(_ change: WaddleEventOverride) -> EventOccurrenceChange? {
        guard let recurrenceStart = moment(change.recurrenceId),
              let start = optionalMoment(change.start),
              let end = optionalMoment(change.end)
        else { return nil }
        return EventOccurrenceChange(
            recurrenceStart: recurrenceStart,
            title: change.summary,
            details: change.description,
            location: change.location,
            start: start,
            end: end
        )
    }
}
