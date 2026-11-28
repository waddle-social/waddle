import SwiftUI
import WaddleKit

/// All day, start and duration or end date.
struct EventWhenCard: View {
    @Binding var form: EventForm

    var body: some View {
        EventEditorCard {
            if form.hasDate {
                Toggle(isOn: $form.isAllDay) {
                    Text("All day")
                        .huddleText(Theme.Typography.label)
                        .foregroundStyle(Theme.Colors.fg)
                }
                .toggleStyle(.huddle)
                .padding(Theme.Spacing.l)
                HuddleDivider()
                EventEditorRow("Starts") {
                    DatePicker("Starts", selection: start, displayedComponents: components)
                        .labelsHidden()
                        .datePickerStyle(.compact)
                }
                HuddleDivider()
                if form.isAllDay {
                    EventEditorRow("Ends") {
                        DatePicker("Ends", selection: $form.lastDay, in: form.start..., displayedComponents: [.date])
                            .labelsHidden()
                            .datePickerStyle(.compact)
                    }
                } else {
                    EventDurationRows(form: $form)
                }
                if form.bringsBackCancelledDates() {
                    HuddleDivider()
                    Label("Moving the start brings back the dates cancelled in this series.", systemImage: "info.circle")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.infoText)
                        .padding(Theme.Spacing.l)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                EventEditorRow("No date yet") {
                    Button("Set a date") { form.hasDate = true }
                        .buttonStyle(.huddle(size: .small))
                }
            }
        }
    }

    private var components: DatePickerComponents {
        form.isAllDay ? [.date] : [.date, .hourAndMinute]
    }

    private var start: Binding<Date> {
        Binding(get: { form.start }, set: { form.moveStart(to: $0) })
    }
}

/// How the event repeats and when that stops.
struct EventRepeatCard: View {
    @Environment(\.eventsCalendar) private var calendar
    @Binding var form: EventForm

    var body: some View {
        let copy = EventsCopy(calendar: calendar)
        EventEditorCard {
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                Text("Repeats")
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fg)
                ViewThatFits(in: .horizontal) {
                    HuddleSegmentedControl(selection: $form.repeats, segments: EventForm.Repeat.allCases.map {
                        .init(value: $0, title: Self.title(of: $0))
                    })
                    repeatMenu
                }
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: .infinity, alignment: .leading)
            if form.repeats != .never {
                HuddleDivider()
                EventEditorRow("Every") {
                    HStack(spacing: Theme.Spacing.s) {
                        EventsStepper(value: $form.interval, range: 1...99, label: "Every")
                        Text(EventsCopy.unit(of: form.repeats, interval: form.interval))
                            .huddleText(Theme.Typography.body)
                            .foregroundStyle(Theme.Colors.fgMuted)
                            .frame(minWidth: 52, alignment: .leading)
                    }
                }
                if form.repeats == .weekly {
                    weekdays(copy)
                }
                HuddleDivider()
                ending
            }
        }
    }

    private var repeatMenu: some View {
        Picker("Repeats", selection: $form.repeats) {
            ForEach(EventForm.Repeat.allCases, id: \.self) { Text(Self.title(of: $0)).tag($0) }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .fixedSize()
    }

    private func weekdays(_ copy: EventsCopy) -> some View {
        let shown = form.shownWeekdays()
        let first = CalendarDay(form.start, in: form.timeZone).weekday
        return HStack(spacing: Theme.Spacing.s) {
            ForEach(copy.week, id: \.self) { day in
                let isOn = shown.contains(day)
                Button {
                    // The day it starts on is always one of them.
                    guard day != first else { return }
                    if form.weekdays.contains(day) { form.weekdays.remove(day) } else { form.weekdays.insert(day) }
                } label: {
                    Text(copy.initial(of: day))
                        .huddleText(Theme.Typography.label.weight(isOn ? 700 : 500))
                        .foregroundStyle(isOn ? Theme.Colors.actionFg : Theme.Colors.fgMuted)
                        .frame(width: 34, height: 34)
                        .background(Circle().fill(isOn ? Theme.Colors.action : Theme.Colors.surface2))
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .huddleTouchTarget()
                .help(day == first ? "\(copy.weekdayName(day)), the day it starts" : copy.weekdayName(day))
                .accessibilityLabel(Text(copy.weekdayName(day)))
                .accessibilityAddTraits(isOn ? .isSelected : [])
            }
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.bottom, Theme.Spacing.m)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var ending: some View {
        let options: [EventForm.Ending] = form.offersEndDate ? [.never, .after, .on] : [.never, .after]
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            Text("Stops")
                .huddleText(Theme.Typography.label)
                .foregroundStyle(Theme.Colors.fg)
            HuddleSegmentedControl(selection: $form.ending, segments: options.map {
                .init(value: $0, title: Self.title(of: $0))
            })
        }
        .padding([.horizontal, .top], Theme.Spacing.l)
        .padding(.bottom, form.ending == .never ? Theme.Spacing.l : Theme.Spacing.xs)
        .frame(maxWidth: .infinity, alignment: .leading)
        switch form.ending {
        case .never:
            EmptyView()
        case .after:
            EventEditorRow("After") {
                HStack(spacing: Theme.Spacing.s) {
                    EventsStepper(value: $form.count, range: 1...999, label: "Times")
                    Text(form.count == 1 ? "time" : "times")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .frame(minWidth: 52, alignment: .leading)
                }
            }
        case .on:
            EventEditorRow("Last date") {
                DatePicker("Last date", selection: $form.until, in: form.start..., displayedComponents: [.date])
                    .labelsHidden()
                    .datePickerStyle(.compact)
            }
        }
    }

    private static func title(of repeats: EventForm.Repeat) -> String {
        switch repeats {
        case .never: "Never"
        case .daily: "Daily"
        case .weekly: "Weekly"
        case .monthly: "Monthly"
        case .yearly: "Yearly"
        }
    }

    private static func title(of ending: EventForm.Ending) -> String {
        switch ending {
        case .never: "Never"
        case .after: "After"
        case .on: "On a date"
        }
    }
}

/// A card of editor rows.
struct EventEditorCard<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(spacing: 0) { content }
            .frame(maxWidth: .infinity, alignment: .leading)
            .huddleSurface(.card, radius: Theme.Radius.card)
            .environment(\.huddleGround, Theme.Colors.surface)
    }
}

/// A label on the leading edge and its control on the trailing one.
struct EventEditorRow<Control: View>: View {
    let title: String
    @ViewBuilder let control: Control

    init(_ title: String, @ViewBuilder control: () -> Control) {
        self.title = title
        self.control = control()
    }

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            Text(title)
                .huddleText(Theme.Typography.label)
                .foregroundStyle(Theme.Colors.fg)
            Spacer(minLength: Theme.Spacing.s)
            control
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.m)
        .frame(minHeight: Theme.Size.minHitTarget)
    }
}

/// A number with − and + beside it.
struct EventsStepper: View {
    @Binding var value: Int
    let range: ClosedRange<Int>
    let label: String

    var body: some View {
        HStack(spacing: 0) {
            step("minus", by: -1)
            Text("\(value)")
                .huddleText(Theme.Typography.label)
                .monospacedDigit()
                .foregroundStyle(Theme.Colors.fg)
                .frame(minWidth: 28)
            step("plus", by: 1)
        }
        .padding(.horizontal, 2)
        .frame(height: WaddleTokens.ControlHeight.sm)
        .background(Capsule().fill(Theme.Colors.surface2))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(label))
        .accessibilityValue(Text("\(value)"))
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: change(by: 1)
            case .decrement: change(by: -1)
            @unknown default: break
            }
        }
    }

    private func step(_ symbol: String, by delta: Int) -> some View {
        Button { change(by: delta) } label: {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(Theme.Colors.fgMuted)
                .frame(width: 28, height: 28)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!range.contains(value + delta))
    }

    private func change(by delta: Int) {
        value = min(max(value + delta, range.lowerBound), range.upperBound)
    }
}
