import Testing
@testable import WaddleKit

struct EventsMonthTests {
    @Test func steppingBetweenDecemberAndJanuaryChangesTheYear() {
        let december = EventsMonth(containing: CalendarDay(year: 2026, month: 12, day: 31))
        let january = EventsMonth(containing: CalendarDay(year: 2027, month: 1, day: 1))

        #expect(december.advanced(by: 1) == january)
        #expect(january.advanced(by: -1) == december)
    }

    @Test func aLeapFebruaryFillsWholeRowsFromThePreferredWeekday() {
        let february = EventsMonth(containing: CalendarDay(year: 2028, month: 2, day: 29))
        let mondayGrid = february.days(firstWeekday: Weekday.monday.rawValue)
        let sundayGrid = february.days(firstWeekday: Weekday.sunday.rawValue)

        #expect(mondayGrid.lowerBound.iso == "2028-01-31")
        #expect(mondayGrid.upperBound.iso == "2028-03-05")
        #expect(sundayGrid.lowerBound.iso == "2028-01-30")
        #expect(sundayGrid.upperBound.iso == "2028-03-04")
    }
}
