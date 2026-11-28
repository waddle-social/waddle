import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for Events: the month with its agenda, seven
/// days with and without events, a repeating event open, and the editor
/// with Weekly picked. These screens have no design frame; they are drawn
/// from the styleguide's components. Dates are shown in Oslo, in British
/// English with weeks starting on Monday, whatever the machine is set to.
/// Compared on the OS the goldens were recorded on (see
/// `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/EventsSnapshotTests` and run the tests once to record
/// new ones (that run fails), then commit them.
@MainActor
final class EventsSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let screen = CGSize.phone
    private static let detail = CGSize.phone
    private static let editor = CGSize.phone
    #else
    private static let screen = CGSize(width: 760, height: 720)
    private static let detail = CGSize(width: 440, height: 680)
    private static let editor = CGSize(width: 460, height: 720)
    #endif

    func testEventsInAMonth() async throws {
        let sample = SampleSession()
        sample.seedEvents()
        try await assertEvents(sample)
    }

    func testEventsWithNothingPlanned() async throws {
        let sample = SampleSession()
        let store = sample.coordinator.events
        _ = store.begin()
        store.finish(failed: false, canManage: false)
        try await assertEvents(sample)
    }

    func testEventsInSevenDays() async throws {
        let sample = SampleSession()
        let store = sample.coordinator.events
        let refresh = store.begin()
        store.publish(events: [
            CommunityEvent(
                id: "offsite", uid: "offsite", title: "Winter offsite", location: "Geilo",
                organizer: SampleSession.priya,
                start: .day(CalendarDay(year: 2027, month: 3, day: 2)),
                end: .day(CalendarDay(year: 2027, month: 3, day: 5))
            ),
            CommunityEvent(
                id: "critique", uid: "critique", title: "Design critique", location: "Studio",
                start: .instant(SampleSession.instant("2027-03-03T10:00:00+01:00"))
            ),
        ], refresh: refresh)
        store.finish(failed: false, canManage: true)
        try await assertEvents(sample, showing: .week)
    }

    func testSevenDaysWithNothingScheduled() async throws {
        let sample = SampleSession()
        let store = sample.coordinator.events
        _ = store.begin()
        store.finish(failed: false, canManage: true)
        try await assertEvents(sample, showing: .week)
    }

    func testSevenDaysWithNoMatchingReplies() async throws {
        let sample = SampleSession()
        sample.seedEvents(canManage: false)
        // Book club falls on the seventh day, and Ana hasn't replied.
        try await assertEvents(
            sample, filtering: .goingOrMaybe, showing: .week,
            clock: SampleSession.instant("2027-04-10T09:00:00+02:00")
        )
    }

    func testAgendaFilteredToGoingOrMaybe() async throws {
        let sample = SampleSession()
        sample.seedEvents()
        try await assertEvents(
            sample, selecting: CalendarDay(year: 2027, month: 3, day: 3), filtering: .goingOrMaybe
        )
    }

    func testAgendaWithNoMatchingReplies() async throws {
        let sample = SampleSession()
        sample.seedEvents(canManage: false)
        try await assertEvents(
            sample, selecting: CalendarDay(year: 2027, month: 4, day: 3), filtering: .goingOrMaybe
        )
    }

    func testAgendaCardWithRSVP() async throws {
        let sample = SampleSession()
        sample.seedEvents(canManage: false)
        let occurrence = SampleSession.gameNight(on: "2027-03-05T19:00:00+01:00", until: "2027-03-05T22:00:00+01:00")
        let day = SampleSession.selectedEventDay
        let now = SampleSession.instant("2027-03-05T12:00:00+01:00")
        let agenda = ScrollView {
            EventsAgenda(
                occurrences: [occurrence], from: day, today: day, now: now,
                undated: [], filter: .all, onOpen: { _ in }
            ) {
                EmptyView()
            }
            .padding(Theme.Spacing.l)
        }
        .background(Theme.Colors.bg)
        try await assertScreenSnapshot(of: pinned(sample.host(agenda)), size: Self.screen)
    }

    func testARepeatingEvent() async throws {
        let sample = SampleSession()
        sample.seedEvents()
        let occurrence = SampleSession.gameNight(on: "2027-03-05T19:00:00+01:00", until: "2027-03-05T22:00:00+01:00")
        let sheet = EventDetailSheet(opened: OpenEvent(eventID: occurrence.eventID, occurrence: occurrence))
        try await assertScreenSnapshot(of: pinned(sample.host(sheet)), size: Self.detail, asSheet: true)
    }

    func testEditingAWeeklyEvent() async throws {
        let sample = SampleSession()
        sample.seedEvents()
        let form = EventForm(editing: SampleSession.gameNight, now: SampleSession.eventsClock, timeZone: SampleSession.oslo)
        XCTAssertEqual(form.repeats, .weekly)
        try await assertScreenSnapshot(
            of: pinned(sample.host(EventEditorSheet(form: form))), size: Self.editor, asSheet: true
        )
    }

    func testAgendaWithEventsUnderwayAndStartingSoon() async throws {
        let sample = SampleSession()
        let store = sample.coordinator.events
        let refresh = store.begin()
        store.publish(events: [
            CommunityEvent(
                id: "critique", uid: "critique", title: "Design critique", location: "Studio",
                start: .instant(SampleSession.instant("2027-03-05T16:00:00+01:00")),
                end: .instant(SampleSession.instant("2027-03-05T18:00:00+01:00"))
            ),
            SampleSession.gameNight,
        ], refresh: refresh)
        store.finish(failed: false, canManage: true)
        sample.port.calendar.occurrences[SampleSession.gameNight.id] = [
            SampleSession.gameNight(on: "2027-03-05T19:00:00+01:00", until: "2027-03-05T22:00:00+01:00"),
        ]
        let events = NavigationStack {
            EventsView(
                clock: SampleSession.instant("2027-03-05T17:00:00+01:00"),
                selecting: SampleSession.selectedEventDay
            )
        }
        try await assertScreenSnapshot(of: pinned(sample.host(events)), size: Self.screen)
    }

    func testNewEventWithTheDefaultDuration() async throws {
        let sample = SampleSession()
        var form = EventForm(on: SampleSession.selectedEventDay, now: SampleSession.eventsClock, timeZone: SampleSession.oslo)
        form.title = "Design critique"
        XCTAssertEqual(form.duration, .thirtyMinutes)
        try await assertScreenSnapshot(
            of: pinned(sample.host(EventEditorSheet(form: form))), size: Self.editor, asSheet: true
        )
    }

    func testEditingAnEventWithACustomDuration() async throws {
        let sample = SampleSession()
        let event = CommunityEvent(
            id: "critique", uid: "critique", title: "Design critique", location: "Studio",
            start: .instant(SampleSession.instant("2027-03-05T10:00:00+01:00")),
            end: .instant(SampleSession.instant("2027-03-05T10:45:00+01:00"))
        )
        let form = EventForm(editing: event, now: SampleSession.eventsClock, timeZone: SampleSession.oslo)
        XCTAssertEqual(form.duration, .custom)
        try await assertScreenSnapshot(
            of: pinned(sample.host(EventEditorSheet(form: form))), size: Self.editor, asSheet: true
        )
    }

    private func assertEvents(
        _ sample: SampleSession,
        selecting day: CalendarDay? = nil,
        filtering filter: EventFilter = .all,
        showing mode: EventsView.Mode = .month,
        clock: Date = SampleSession.eventsClock,
        testName: String = #function
    ) async throws {
        let events = NavigationStack {
            EventsView(clock: clock, selecting: day ?? SampleSession.selectedEventDay, filtering: filter, showing: mode)
        }
        try await assertScreenSnapshot(of: pinned(sample.host(events)), size: Self.screen, testName: testName)
    }

    /// Oslo, British English, weeks from Monday.
    private func pinned<V: View>(_ view: V) -> some View {
        var calendar = Calendar(identifier: .gregorian)
        calendar.firstWeekday = 2
        calendar.minimumDaysInFirstWeek = 4
        return view
            .environment(\.calendar, calendar)
            .environment(\.timeZone, SampleSession.oslo)
            .environment(\.locale, Locale(identifier: "en_GB"))
    }
}
