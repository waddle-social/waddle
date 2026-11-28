import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@Suite("Calendar event filter")
struct EventFilterTests {
    @Test func goingOrMaybeKeepsOnlyMyMatchingReplies() {
        let account = me.jid
        let events = [
            event("someone-else-going", attendees: [EventAttendee(jid: bob, reply: .going)]),
            event("maybe", attendees: [EventAttendee(jid: account, reply: .maybe)]),
            event("declined", attendees: [
                EventAttendee(jid: bob, reply: .maybe),
                EventAttendee(jid: account, reply: .declined),
            ]),
            event("no-reply"),
            event("going", attendees: [EventAttendee(jid: account, reply: .going)]),
        ]

        #expect(EventFilter.goingOrMaybe.apply(to: events, for: account).map(\.id) == ["maybe", "going"])
        #expect(EventFilter.all.apply(to: events, for: account) == events)
    }

    @MainActor
    @Test func goingOrMaybeFollowsTheReplyForEveryDateOfASeries() async throws {
        let port = FakePort()
        let session = SessionCoordinator(account: me, port: port)
        session.status.connection = .online
        let start = date(0)
        let nextStart = date(604_800)
        let series = CommunityEvent(
            id: "series", uid: "series", title: "Game night", start: .instant(start),
            attendees: [EventAttendee(jid: me.jid, reply: .maybe)],
            recurrence: EventRecurrence(frequency: .weekly)
        )
        let critique = CommunityEvent(id: "critique", uid: "critique", title: "Design critique", start: .instant(date(3600)))
        port.calendar.list = .success([series, critique])
        port.calendar.occurrences[series.id] = [start, nextStart].map {
            EventOccurrence(eventID: series.id, recurrenceStart: .instant($0), start: .instant($0), end: nil, title: series.title)
        }
        await session.refreshEvents()
        let range = date(-1)..<date(604_801)
        let utc = try #require(TimeZone(identifier: "UTC"))

        let before = session.eventOccurrences(in: range, timeZone: utc, filter: .goingOrMaybe)
        #expect(before.map(\.start) == [.instant(start), .instant(nextStart)])

        try await session.reply(.declined, to: series.id)
        #expect(session.eventOccurrences(in: range, timeZone: utc, filter: .goingOrMaybe).isEmpty)
        #expect(session.eventOccurrences(in: range, timeZone: utc).map(\.eventID) == ["series", "critique", "series"])
    }

    private func event(_ id: String, attendees: [EventAttendee] = []) -> CommunityEvent {
        CommunityEvent(id: id, uid: id, title: id, attendees: attendees)
    }
}
