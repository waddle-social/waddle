import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private let now = date(1000)
private let utc = TimeZone(identifier: "UTC")!

private func event(
    _ id: String,
    at seconds: TimeInterval? = 0,
    replies: [EventAttendee] = [],
    repeats: EventRecurrence? = nil
) -> CommunityEvent {
    CommunityEvent(
        id: id,
        uid: "uid-\(id)",
        title: "Event \(id)",
        organizer: bob,
        start: seconds.map { .instant(date($0)) },
        end: seconds.map { .instant(date($0 + 3600)) },
        attendees: replies,
        recurrence: repeats
    )
}

private func draft(_ title: String, id: String? = nil) -> EventDraft {
    EventDraft(eventID: id, title: title, start: .instant(date(5000)), end: .instant(date(8600)))!
}

@MainActor
@Suite("Community events")
struct EventsTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    // MARK: Loading

    @Test func refreshLoadsTheEventsAndWhetherICanManageThem() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a"), event("b")])
        port.calendar.isOwner = true

        await coordinator.refreshEvents(at: now)

        #expect(coordinator.events.events.map(\.id) == ["a", "b"])
        #expect(coordinator.events.canManage)
        #expect(coordinator.events.hasLoaded)
        #expect(coordinator.events.loadedAt == now)
        #expect(!coordinator.events.isLoading)
        #expect(!coordinator.events.failed)
    }

    @Test func aMemberCannotManageEvents() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])

        await coordinator.refreshEvents(at: now)

        #expect(!coordinator.events.canManage)
    }

    @Test func aFailedRefreshKeepsTheEventsShown() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)

        port.calendar.list = .failure(.timeout)
        await coordinator.refreshEvents(at: now)

        #expect(coordinator.events.failed)
        #expect(coordinator.events.events.map(\.id) == ["a"])
    }

    @Test func refreshOfflineAsksNothing() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)

        await coordinator.refreshEvents(at: now)

        #expect(port.calendar.fetchRequests == 0)
        #expect(!coordinator.events.hasLoaded)
        #expect(!coordinator.events.isLoading)
    }

    @Test func anAnswerThatArrivesAfterSignOutIsDropped() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        port.calendar.held = ["fetchEvents"]

        let refresh = Task { await coordinator.refreshEvents(at: now) }
        while port.calendar.fetchRequests == 0 { await Task.yield() }
        await coordinator.stop()
        port.calendar.release()
        await refresh.value

        #expect(coordinator.events.events.isEmpty)
        #expect(!coordinator.events.hasLoaded)
    }

    @Test func aNewerRefreshWins() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("old")])
        port.calendar.held = ["fetchEvents"]
        let first = Task { await coordinator.refreshEvents(at: now) }
        while port.calendar.fetchRequests == 0 { await Task.yield() }

        port.calendar.held = []
        port.calendar.list = .success([event("new")])
        await coordinator.refreshEvents(at: now)
        port.calendar.release()
        await first.value

        #expect(coordinator.events.events.map(\.id) == ["new"])
    }

    @Test func signingOutForgetsEverything() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        port.calendar.isOwner = true
        await coordinator.refreshEvents(at: now)

        await coordinator.stop()

        #expect(coordinator.events.events.isEmpty)
        #expect(!coordinator.events.canManage)
        #expect(!coordinator.events.hasLoaded)
        #expect(coordinator.events.loadedAt == nil)
    }

    // MARK: Occurrences

    @Test func occurrencesInARangeAreSortedAcrossEvents() async {
        let (coordinator, port) = online()
        let weekly = EventRecurrence(frequency: .weekly)
        port.calendar.list = .success([event("late", at: 7200), event("series", at: 0, repeats: weekly), event("undated", at: nil)])
        port.calendar.occurrences["series"] = [
            EventOccurrence(eventID: "series", recurrenceStart: .instant(date(0)), start: .instant(date(0)), end: nil, title: "Series"),
            EventOccurrence(eventID: "series", recurrenceStart: .instant(date(604_800)), start: .instant(date(604_800)), end: nil, title: "Series"),
        ]
        await coordinator.refreshEvents(at: now)

        let shown = coordinator.eventOccurrences(in: date(-10)..<date(86400), timeZone: utc)

        #expect(shown.map(\.eventID) == ["series", "late"])
        #expect(coordinator.events.undated.map(\.id) == ["undated"])
    }

    @Test func allDayOccurrencesComeFirstOnTheirDay() async {
        let (coordinator, port) = online()
        let morning = CommunityEvent(id: "m", uid: "m", title: "Breakfast", start: .instant(at("2027-02-05T08:00:00Z")))
        let holiday = CommunityEvent(
            id: "h", uid: "h", title: "Holiday",
            start: .day(CalendarDay(year: 2027, month: 2, day: 5)), end: .day(CalendarDay(year: 2027, month: 2, day: 6))
        )
        let before = CommunityEvent(id: "b", uid: "b", title: "Day before", start: .day(CalendarDay(year: 2027, month: 2, day: 4)))
        port.calendar.list = .success([morning, holiday, before])
        await coordinator.refreshEvents(at: now)

        let shown = coordinator.eventOccurrences(in: at("2027-02-05T00:00:00Z")..<at("2027-02-06T00:00:00Z"), timeZone: utc)

        #expect(shown.map(\.eventID) == ["h", "m"])
    }

    // MARK: Saving and cancelling

    @Test func aNewEventIsAddedToTheList() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)

        let saved = try await coordinator.saveEvent(draft("Game night"))

        #expect(port.calendar.published.map(\.title) == ["Game night"])
        #expect(coordinator.events.events.map(\.id) == ["a", saved.id])
    }

    @Test func anEditedEventReplacesTheOneShown() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a"), event("b")])
        await coordinator.refreshEvents(at: now)

        try await coordinator.saveEvent(draft("Renamed", id: "a"))

        #expect(coordinator.events.events.map(\.id) == ["a", "b"])
        #expect(coordinator.events.event("a")?.title == "Renamed")
    }

    @Test func aRefusedSaveLeavesTheListAlone() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        port.calendar.publishResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) {
            try await coordinator.saveEvent(draft("Renamed", id: "a"))
        }
        #expect(coordinator.events.event("a")?.title == "Event a")
    }

    @Test func savingOfflineFailsWithoutAsking() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)

        await #expect(throws: PortError.notConnected) {
            try await coordinator.saveEvent(draft("Game night"))
        }
        #expect(port.calendar.published.isEmpty)
    }

    @Test func anEventSavedDuringARefreshSurvivesItsOlderAnswer() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        port.calendar.held = ["fetchEvents"]
        let refresh = Task { await coordinator.refreshEvents(at: now) }
        while port.calendar.fetchRequests == 0 { await Task.yield() }

        let saved = try await coordinator.saveEvent(draft("Game night"))
        port.calendar.release()
        await refresh.value

        #expect(coordinator.events.events.map(\.id) == ["a", saved.id])
    }

    @Test func cancellingAnEventRemovesIt() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a"), event("b")])
        await coordinator.refreshEvents(at: now)

        try await coordinator.cancelEvent("a")

        #expect(port.calendar.retracted == ["a"])
        #expect(coordinator.events.events.map(\.id) == ["b"])
    }

    @Test func anEventCancelledDuringARefreshStaysGone() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a"), event("b")])
        await coordinator.refreshEvents(at: now)
        port.calendar.held = ["fetchEvents"]
        let refresh = Task { await coordinator.refreshEvents(at: now) }
        while port.calendar.fetchRequests == 1 { await Task.yield() }

        try await coordinator.cancelEvent("a")
        port.calendar.release()
        await refresh.value

        #expect(coordinator.events.events.map(\.id) == ["b"])
    }

    @Test func aRefusedCancelKeepsTheEvent() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        port.calendar.cancelResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) { try await coordinator.cancelEvent("a") }
        #expect(coordinator.events.events.map(\.id) == ["a"])
    }

    @Test func cancellingOneDateOfASeriesShowsTheSeriesTheServerReturns() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("s", repeats: EventRecurrence(frequency: .daily))])
        await coordinator.refreshEvents(at: now)
        let second = EventOccurrence(
            eventID: "s", recurrenceStart: .instant(date(86400)), start: .instant(date(86400)), end: nil, title: "Event s"
        )

        try await coordinator.cancelOccurrence(second)

        #expect(port.calendar.cancelledOccurrences.map(\.start) == [.instant(date(86400))])
        #expect(coordinator.events.event("s")?.cancelled == [.instant(date(86400))])
    }

    @Test func anEventThatDoesNotRepeatHasNoDateToCancel() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        let only = EventOccurrence(eventID: "a", recurrenceStart: nil, start: .instant(date(0)), end: nil, title: "Event a")

        await #expect(throws: PortError.invalidRequest) { try await coordinator.cancelOccurrence(only) }
        #expect(port.calendar.cancelledOccurrences.isEmpty)
    }

    // MARK: Replies

    @Test func agendaRSVPShowsTheWebChoicesAndCounts() {
        let shown = event("a", replies: [
            EventAttendee(jid: bob, reply: .going),
            EventAttendee(jid: me.jid, reply: .maybe),
            EventAttendee(jid: bare("carol@waddle.test"), reply: .going),
            EventAttendee(jid: bare("dave@waddle.test"), reply: .declined),
        ])
        let rsvp = EventRSVP(event: shown, viewer: me.jid)

        #expect(EventReply.allCases.map(EventRSVP.choiceLabel) == ["Going", "Maybe", "Can't"])
        #expect(rsvp.selection == .maybe)
        #expect(rsvp.countsLabel == "2 going · 1 maybe · 1 can't")

        let empty = EventRSVP(event: event("b"), viewer: me.jid)
        #expect(empty.selection == nil)
        #expect(empty.countsLabel == "0 going · 0 maybe · 0 can't")
    }

    @Test func aReplyShowsAtOnceAndIsSentAsMine() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a", replies: [EventAttendee(jid: bob, reply: .going)])])
        await coordinator.refreshEvents(at: now)
        port.calendar.held = ["reply"]

        let reply = Task { try await coordinator.reply(.maybe, to: "a") }
        while port.calendar.replies.isEmpty { await Task.yield() }

        #expect(coordinator.events.myReply(to: "a") == .maybe)
        #expect(coordinator.events.event("a")?.attendees == [
            EventAttendee(jid: bob, reply: .going), EventAttendee(jid: me.jid, reply: .maybe),
        ])
        #expect(coordinator.events.event("a").map { EventRSVP(event: $0, viewer: me.jid).countsLabel }
            == "1 going · 1 maybe · 0 can't")
        port.calendar.release()
        try await reply.value
        #expect(port.calendar.replies.map(\.uid) == ["uid-a"])
        #expect(port.calendar.replies.map(\.reply) == [.maybe])
    }

    @Test func aRefusedReplyIsUndone() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a", replies: [EventAttendee(jid: me.jid, reply: .going)])])
        await coordinator.refreshEvents(at: now)
        port.calendar.replyResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) { try await coordinator.reply(.declined, to: "a") }

        #expect(coordinator.events.myReply(to: "a") == .going)
        #expect(coordinator.events.event("a").map { EventRSVP(event: $0, viewer: me.jid).countsLabel }
            == "1 going · 0 maybe · 0 can't")
    }

    @Test func aRefusedFirstReplyLeavesNoReply() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        port.calendar.replyResult = .failure(.timeout)

        await #expect(throws: PortError.timeout) { try await coordinator.reply(.going, to: "a") }

        #expect(coordinator.events.myReply(to: "a") == nil)
        #expect(coordinator.events.event("a")?.attendees == [])
    }

    @Test func tappingTheCurrentReplyKeepsItWithoutAsking() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a", replies: [EventAttendee(jid: me.jid, reply: .going)])])
        await coordinator.refreshEvents(at: now)

        try await coordinator.reply(.going, to: "a")

        #expect(port.calendar.replies.isEmpty)
        #expect(coordinator.events.myReply(to: "a") == .going)
    }

    @Test func aRefreshDoesNotUndoAReplyStillBeingSent() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        port.calendar.held = ["reply"]
        let reply = Task { try await coordinator.reply(.going, to: "a") }
        while port.calendar.replies.isEmpty { await Task.yield() }

        await coordinator.refreshEvents(at: now)

        #expect(coordinator.events.myReply(to: "a") == .going)
        port.calendar.release()
        try await reply.value
        #expect(coordinator.events.myReply(to: "a") == .going)
    }

    @Test func aReplySentDuringARefreshSurvivesItsOlderAnswer() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        port.calendar.held = ["fetchEvents"]
        let refresh = Task { await coordinator.refreshEvents(at: now) }
        while port.calendar.fetchRequests == 1 { await Task.yield() }

        try await coordinator.reply(.going, to: "a")
        port.calendar.release()
        await refresh.value

        #expect(coordinator.events.myReply(to: "a") == .going)
    }

    @Test func aLaterRefreshShowsTheRepliesTheServerHolds() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        try await coordinator.reply(.going, to: "a")

        port.calendar.list = .success([event("a", replies: [EventAttendee(jid: me.jid, reply: .maybe)])])
        await coordinator.refreshEvents(at: now)

        #expect(coordinator.events.myReply(to: "a") == .maybe)
    }

    @Test func aRefusedReplyStaysUndoneWhenTheEventWasSavedMeanwhile() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        port.calendar.held = ["fetchEvents", "reply"]
        let refresh = Task { await coordinator.refreshEvents(at: now) }
        while port.calendar.fetchRequests == 1 { await Task.yield() }
        let reply = Task { try await coordinator.reply(.going, to: "a") }
        while port.calendar.replies.isEmpty { await Task.yield() }

        try await coordinator.saveEvent(draft("Game night", id: "a"))
        #expect(coordinator.events.myReply(to: "a") == .going)
        port.calendar.replyResult = .failure(.rejected)
        port.calendar.release()
        await #expect(throws: PortError.rejected) { try await reply.value }
        await refresh.value

        #expect(coordinator.events.myReply(to: "a") == nil)
        #expect(coordinator.events.event("a")?.title == "Game night")
    }

    @Test func quickRepliesAreSentInOrderAndEndAsTheLastOne() async throws {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        port.calendar.held = ["reply"]

        let first = Task { try await coordinator.reply(.going, to: "a") }
        while port.calendar.replies.isEmpty { await Task.yield() }
        let second = Task { try await coordinator.reply(.declined, to: "a") }
        for _ in 0..<20 { await Task.yield() }

        #expect(port.calendar.replies.map(\.reply) == [.going])
        port.calendar.held = []
        port.calendar.release()
        try await first.value
        try await second.value

        #expect(port.calendar.replies.map(\.reply) == [.going, .declined])
        #expect(coordinator.events.myReply(to: "a") == .declined)
    }

    @Test func replyingOfflineFails() async {
        let (coordinator, port) = online()
        port.calendar.list = .success([event("a")])
        await coordinator.refreshEvents(at: now)
        coordinator.status.connection = .offline(retryAt: nil)

        await #expect(throws: PortError.notConnected) { try await coordinator.reply(.going, to: "a") }
        #expect(coordinator.events.myReply(to: "a") == nil)
    }

    @Test func repliesAreCountedByKind() {
        let replies = [
            EventAttendee(jid: bob, reply: .going),
            EventAttendee(jid: me.jid, reply: .going),
            EventAttendee(jid: bare("carol@waddle.test"), reply: .declined),
        ]
        let shown = event("a", replies: replies)

        #expect(shown.attendees(replying: .going).map(\.jid) == [bob, me.jid])
        #expect(shown.attendees(replying: .maybe).isEmpty)
        #expect(shown.attendees(replying: .declined).map(\.jid) == [bare("carol@waddle.test")])
    }
}

/// An instant written as ISO 8601 with its offset.
private func at(_ iso: String) -> Date {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime]
    return formatter.date(from: iso)!
}
