import Foundation
@testable import Waddle
@testable import WaddleKit

/// A month of community events in March 2027, seen from Oslo on Wednesday
/// 3 March: a fortnightly game night, a one-off critique, a check-in with
/// no end, a three-day offsite, a monthly book club and a trip with no
/// date yet. The fake port cannot walk a rule, so each series' dates are
/// scripted as the core gives them: in UTC, so the game night moves an
/// hour later on the clock once summer time starts on 28 March.
extension SampleSession {
    static let oslo = TimeZone(identifier: "Europe/Oslo")!
    /// Wednesday 3 March 2027, 09:00 in Oslo.
    static let eventsClock = instant("2027-03-03T09:00:00+01:00")
    /// Friday 5 March 2027.
    static let selectedEventDay = CalendarDay(year: 2027, month: 3, day: 5)

    static let gameNight = CommunityEvent(
        id: "game-night",
        uid: "game-night",
        title: "Game night",
        details: "Bring a game or borrow one of ours. Snacks are on the house.",
        location: "The Lounge",
        organizer: priya,
        start: .instant(instant("2027-03-05T19:00:00+01:00")),
        end: .instant(instant("2027-03-05T22:00:00+01:00")),
        attendees: [
            EventAttendee(jid: priya, reply: .going),
            EventAttendee(jid: ana.jid, reply: .going),
            EventAttendee(jid: mika, reply: .going),
            EventAttendee(jid: jo, reply: .maybe),
            EventAttendee(jid: theo, reply: .declined),
        ],
        recurrence: EventRecurrence(frequency: .weekly, interval: 2, weekdays: [.friday], end: .count(10))
    )

    static func gameNight(on start: String, until end: String) -> EventOccurrence {
        EventOccurrence(
            eventID: gameNight.id,
            recurrenceStart: .instant(instant(start)),
            start: .instant(instant(start)),
            end: .instant(instant(end)),
            title: gameNight.title,
            details: gameNight.details,
            location: gameNight.location
        )
    }

    /// Fills the calendar as a refresh would, for an owner when `canManage`.
    func seedEvents(canManage: Bool = true) {
        let bookClub = CommunityEvent(
            id: "book-club",
            uid: "book-club",
            title: "Book club",
            location: "Library corner",
            organizer: Self.theo,
            start: .instant(Self.instant("2027-03-16T18:30:00+01:00")),
            end: .instant(Self.instant("2027-03-16T20:00:00+01:00")),
            attendees: [EventAttendee(jid: Self.jo, reply: .going)],
            recurrence: EventRecurrence(frequency: .monthly)
        )
        let events = [
            Self.gameNight,
            CommunityEvent(
                id: "critique",
                uid: "critique",
                title: "Design critique",
                location: "Studio",
                organizer: Self.jo,
                start: .instant(Self.instant("2027-03-03T14:00:00+01:00")),
                end: .instant(Self.instant("2027-03-03T15:00:00+01:00"))
            ),
            CommunityEvent(
                id: "check-in",
                uid: "check-in",
                title: "Release check-in",
                organizer: Self.mika,
                start: .instant(Self.instant("2027-03-05T10:00:00+01:00")),
                attendees: [EventAttendee(jid: Self.ana.jid, reply: .maybe)]
            ),
            CommunityEvent(
                id: "offsite",
                uid: "offsite",
                title: "Winter offsite",
                location: "Geilo",
                organizer: Self.priya,
                start: .day(CalendarDay(year: 2027, month: 3, day: 10)),
                end: .day(CalendarDay(year: 2027, month: 3, day: 13)),
                attendees: [EventAttendee(jid: Self.ana.jid, reply: .going)]
            ),
            bookClub,
            CommunityEvent(id: "spring-trip", uid: "spring-trip", title: "Spring trip", location: "Somewhere warm"),
        ]
        port.calendar.occurrences[Self.gameNight.id] = [
            Self.gameNight(on: "2027-03-05T19:00:00+01:00", until: "2027-03-05T22:00:00+01:00"),
            Self.gameNight(on: "2027-03-19T19:00:00+01:00", until: "2027-03-19T22:00:00+01:00"),
            Self.gameNight(on: "2027-04-02T20:00:00+02:00", until: "2027-04-02T23:00:00+02:00"),
        ]
        port.calendar.occurrences[bookClub.id] = ["2027-03-16T18:30:00+01:00", "2027-04-16T19:30:00+02:00"].map { start in
            EventOccurrence(
                eventID: bookClub.id,
                recurrenceStart: .instant(Self.instant(start)),
                start: .instant(Self.instant(start)),
                end: .instant(Self.instant(start).addingTimeInterval(90 * 60)),
                title: bookClub.title,
                location: bookClub.location
            )
        }
        let store = coordinator.events
        let refresh = store.begin()
        store.publish(events: events, refresh: refresh)
        store.finish(failed: false, canManage: canManage)
    }

    static func instant(_ iso: String) -> Date {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: iso)!
    }
}
