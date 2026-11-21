import WaddleKit
import XCTest
@testable import Waddle

/// The dates of an event, through the bridge and the Rust core that works
/// them out. No network: `expandCommunityEvent` is a pure function.
final class EventsBridgeTests: XCTestCase {
    /// 2027-02-05T19:00:00Z, a Friday.
    private let friday = Date(timeIntervalSince1970: 1_801_854_000)
    private let hour: TimeInterval = 3600
    private let week: TimeInterval = 7 * 24 * 3600

    private func dates(of event: CommunityEvent, weeks: Double) throws -> [EventOccurrence] {
        try expandCommunityEvent(
            event: FFIOutbound.event(event),
            fromMs: FFIOutbound.milliseconds(friday),
            toMs: FFIOutbound.milliseconds(friday + weeks * week)
        ).compactMap { FFIInbound.occurrence($0, of: event.id) }
    }

    func testAWeeklyEventKeepsItsCancelledAndChangedDates() throws {
        let gameNight = CommunityEvent(
            id: "night",
            uid: "night",
            title: "Game night",
            location: "The Lounge",
            start: .instant(friday),
            end: .instant(friday + 3 * hour),
            recurrence: EventRecurrence(frequency: .weekly, weekdays: [.friday], end: .count(4)),
            cancelled: [.instant(friday + week)],
            changed: [
                EventOccurrenceChange(
                    recurrenceStart: .instant(friday + 2 * week),
                    location: "Room 2",
                    start: .instant(friday + 2 * week + hour)
                ),
            ]
        )

        let found = try dates(of: gameNight, weeks: 10)

        XCTAssertEqual(found.map(\.start), [
            .instant(friday),
            .instant(friday + 2 * week + hour),
            .instant(friday + 3 * week),
        ])
        XCTAssertEqual(found.map(\.location), ["The Lounge", "Room 2", "The Lounge"])
        XCTAssertEqual(found.map(\.title), ["Game night", "Game night", "Game night"])
        XCTAssertEqual(found[1].recurrenceStart, .instant(friday + 2 * week), "the date to cancel it by")
        XCTAssertEqual(found[1].end, .instant(friday + 2 * week + 4 * hour), "a moved date keeps the length")
        XCTAssertEqual(found.map(\.eventID), ["night", "night", "night"])
    }

    func testAnAllDayEventComesBackAsDays() throws {
        let offsite = CommunityEvent(
            id: "offsite",
            uid: "offsite",
            title: "Offsite",
            start: .day(CalendarDay(year: 2027, month: 2, day: 5)),
            end: .day(CalendarDay(year: 2027, month: 2, day: 7)),
            recurrence: EventRecurrence(frequency: .monthly, end: .count(2))
        )

        let found = try dates(of: offsite, weeks: 10)

        XCTAssertEqual(found.map(\.start), [
            .day(CalendarDay(year: 2027, month: 2, day: 5)),
            .day(CalendarDay(year: 2027, month: 3, day: 5)),
        ])
        XCTAssertEqual(found.last?.end, .day(CalendarDay(year: 2027, month: 3, day: 7)))
    }

    func testAnEventThatDoesNotRepeatHasOneDateAndNoneToCancel() throws {
        let launch = CommunityEvent(id: "launch", uid: "launch", title: "Launch", start: .instant(friday + hour))

        let found = try dates(of: launch, weeks: 1)

        XCTAssertEqual(found.map(\.start), [.instant(friday + hour)])
        XCTAssertEqual(found.first?.recurrenceStart, .some(nil))
        XCTAssertNil(found.first?.end)
    }
}
