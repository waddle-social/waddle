import SwiftUI
import WaddleKit

/// A month to pick a day in. Days with something on have a dot for each
/// event, up to three, or a count.
struct EventsMonthCard: View {
    @Environment(\.eventsCalendar) private var calendar
    let month: EventsMonth
    let days: ClosedRange<CalendarDay>
    let today: CalendarDay
    let selection: CalendarDay
    let occurrences: [EventOccurrence]
    let onSelect: (CalendarDay) -> Void
    /// Moves the month shown by this many months.
    let onMove: (Int) -> Void
    let onToday: () -> Void

    private var copy: EventsCopy { EventsCopy(calendar: calendar) }
    private let columns = Array(repeating: GridItem(.flexible(), spacing: 0), count: 7)

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            header
            LazyVGrid(columns: columns, spacing: Theme.Spacing.xxs) {
                ForEach(copy.week, id: \.self) { weekday in
                    Text(copy.shortWeekdayName(weekday))
                        .huddleText(Theme.Typography.tag)
                        .foregroundStyle(Theme.Colors.fgSoft)
                        .frame(maxWidth: .infinity)
                        .padding(.bottom, Theme.Spacing.xs)
                        .accessibilityHidden(true)
                }
                ForEach(Array(days), id: \.self) { day in
                    dayCell(day, count: count(on: day))
                }
            }
        }
        .padding(Theme.Spacing.l)
        .huddleSurface(.card, radius: Theme.Radius.card)
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    private var header: some View {
        HStack(spacing: Theme.Spacing.xs) {
            Text(copy.monthTitle(year: month.year, month: month.month))
                .huddleText(Theme.Typography.subtitle)
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(1)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: Theme.Spacing.s)
            Button("Today", action: onToday)
                .buttonStyle(.huddle(size: .small))
                .disabled(selection == today && EventsMonth(containing: today) == month)
            stepButton("chevron.left", label: "Previous month") { onMove(-1) }
            stepButton("chevron.right", label: "Next month") { onMove(1) }
        }
    }

    private func stepButton(_ symbol: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 13, weight: .semibold))
                .frame(width: WaddleTokens.ControlHeight.md, height: WaddleTokens.ControlHeight.md)
                .foregroundStyle(Theme.Colors.fgMuted)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .huddleTouchTarget()
        .help(label)
        .accessibilityLabel(Text(label))
    }

    private func count(on day: CalendarDay) -> Int {
        occurrences.reduce(0) { $0 + ($1.days(in: calendar.timeZone).contains(day) ? 1 : 0) }
    }

    private func dayCell(_ day: CalendarDay, count: Int) -> some View {
        let isSelected = day == selection
        let isToday = day == today
        let inMonth = day.month == month.month
        return Button {
            onSelect(day)
        } label: {
            VStack(spacing: 3) {
                Text("\(day.day)")
                    .huddleText(Theme.Typography.label.weight(isSelected || isToday ? 700 : 500))
                    .monospacedDigit()
                    .foregroundStyle(numberColor(isSelected: isSelected, isToday: isToday, inMonth: inMonth))
                    .frame(width: Self.disc, height: Self.disc)
                    .background {
                        if isSelected {
                            Circle().fill(Theme.Colors.action)
                        } else if isToday {
                            Circle().strokeBorder(Theme.Colors.action, lineWidth: WaddleTokens.BorderWidth.strong)
                        }
                    }
                marks(count, inMonth: inMonth)
            }
            .frame(maxWidth: .infinity, minHeight: Self.cellHeight)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(accessibilityLabel(day, count: count, isToday: isToday)))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    @ViewBuilder
    private func marks(_ count: Int, inMonth: Bool) -> some View {
        let color = inMonth ? Theme.Colors.action : Theme.Colors.fgSoft
        Group {
            if count > 3 {
                Text("\(count)")
                    .huddleText(Theme.Typography.badge)
                    .monospacedDigit()
                    .foregroundStyle(color)
            } else {
                HStack(spacing: 3) {
                    ForEach(0..<count, id: \.self) { _ in
                        Circle().fill(color).frame(width: 5, height: 5)
                    }
                }
            }
        }
        .frame(height: 12)
        .accessibilityHidden(true)
    }

    private func numberColor(isSelected: Bool, isToday: Bool, inMonth: Bool) -> Color {
        if isSelected { return Theme.Colors.actionFg }
        if isToday { return Theme.Colors.action }
        return inMonth ? Theme.Colors.fg : Theme.Colors.fgSoft
    }

    private func accessibilityLabel(_ day: CalendarDay, count: Int, isToday: Bool) -> String {
        let events = switch count {
        case 0: "nothing planned"
        case 1: "1 event"
        default: "\(count) events"
        }
        return "\(isToday ? "Today, " : "")\(copy.dayTitle(day)), \(events)"
    }

    private static var disc: CGFloat { 32 }
    private static var cellHeight: CGFloat { 44 }
}
