import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for Events: the month with its agenda, the
/// calendar with nothing planned, a repeating event open, and the editor
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

    private func assertEvents(_ sample: SampleSession, testName: String = #function) async throws {
        let events = NavigationStack {
            EventsView(clock: SampleSession.eventsClock, selecting: SampleSession.selectedEventDay)
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
