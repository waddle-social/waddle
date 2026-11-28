import Foundation
import Testing
import WaddleKit

struct EventDurationTests {
    private let day = CalendarDay(year: 2027, month: 2, day: 5)

    @Test func aNewTimedEventDefaultsToThirtyMinutes() throws {
        let form = newForm()

        #expect(form.duration == .thirtyMinutes)
        #expect(try #require(form.draft()).end == .instant(at("2027-02-05T10:30:00Z")))
    }

    @Test func theChoicesMatchTheWebInOrder() {
        #expect(EventDuration.allCases.map(\.label) == [
            "30 minutes", "1 hour", "90 minutes", "2 hours", "3 hours", "Custom",
        ])
    }

    @Test(arguments: [
        (EventDuration.thirtyMinutes, "2027-02-05T23:30:00Z"),
        (.oneHour, "2027-02-06T00:00:00Z"),
        (.ninetyMinutes, "2027-02-06T00:30:00Z"),
        (.twoHours, "2027-02-06T01:00:00Z"),
        (.threeHours, "2027-02-06T02:00:00Z"),
    ])
    func aPresetKeepsItsLengthWhenTheStartMoves(choice: EventDuration, expectedEnd: String) throws {
        var form = newForm()
        form.duration = choice
        form.moveStart(to: at("2027-02-05T23:00:00Z"))

        let draft = try #require(form.draft())
        #expect(draft.start == .instant(at("2027-02-05T23:00:00Z")))
        #expect(draft.end == .instant(at(expectedEnd)))
    }

    @Test func aPresetUsesElapsedMinutesAcrossAClockChange() throws {
        var form = newForm()
        form.duration = .oneHour
        form.moveStart(to: at("2027-03-28T01:30:00+01:00"))

        #expect(try #require(form.draft()).end == .instant(at("2027-03-28T03:30:00+02:00")))
    }

    @Test func editingRoundsElapsedMinutesToChooseAPreset() throws {
        let form = editing(endingAt: .instant(at("2027-02-05T11:29:30Z")))

        #expect(form.duration == .ninetyMinutes)
        #expect(try #require(form.draft()).end == .instant(at("2027-02-05T11:30:00Z")))
    }

    @Test func editingWithNoTimedEndDefaultsToThirtyMinutes() throws {
        for end in [nil, .day(day)] as [CalendarMoment?] {
            let form = editing(endingAt: end)

            #expect(form.duration == .thirtyMinutes)
            #expect(try #require(form.draft()).end == .instant(at("2027-02-05T10:30:00Z")))
        }
    }

    @Test func aCustomEndStaysWhereItWasPickedWhenTheStartMoves() throws {
        let end = at("2027-02-05T10:45:00Z")
        var form = editing(endingAt: .instant(end))
        #expect(form.duration == .custom)

        form.duration = .oneHour
        form.moveStart(to: at("2027-02-05T10:15:00Z"))
        form.duration = .custom

        #expect(try #require(form.draft()).end == .instant(end))
        form.moveStart(to: end)
        #expect(form.draft() == nil)
    }

    @Test func aNewCustomDurationNeedsAnEndTime() throws {
        var form = newForm()
        form.duration = .custom
        #expect(form.draft() == nil)

        let end = at("2027-02-05T10:45:00Z")
        form.customEnd = end

        #expect(try #require(form.draft()).end == .instant(end))
    }

    @Test func anAllDayEventUsesItsLastDayRegardlessOfTimedDuration() throws {
        var form = newForm()
        form.duration = .custom
        form.isAllDay = true
        form.lastDay = at("2027-02-07T10:00:00Z")

        #expect(try #require(form.draft()).end == .day(CalendarDay(year: 2027, month: 2, day: 8)))
    }

    private func newForm() -> EventForm {
        var form = EventForm(on: day, now: at("2027-02-01T09:00:00Z"), timeZone: .gmt)
        form.title = "Game night"
        return form
    }

    private func editing(endingAt end: CalendarMoment?) -> EventForm {
        let event = CommunityEvent(
            id: "game-night", uid: "game-night", title: "Game night",
            start: .instant(at("2027-02-05T10:00:00Z")), end: end
        )
        return EventForm(editing: event, now: at("2027-02-01T09:00:00Z"), timeZone: .gmt)
    }

    private func at(_ iso: String) -> Date {
        ISO8601DateFormatter().date(from: iso)!
    }
}
