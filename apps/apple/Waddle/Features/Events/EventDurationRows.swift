import SwiftUI
import WaddleKit

/// A timed event's preset and the end time shown for Custom.
struct EventDurationRows: View {
    @Binding var form: EventForm

    var body: some View {
        EventEditorRow("Duration") {
            Menu {
                Picker("Duration", selection: $form.duration) {
                    ForEach(EventDuration.allCases, id: \.self) { choice in
                        Text(choice.label).tag(choice)
                    }
                }
                .pickerStyle(.inline)
            } label: {
                Label(form.duration.label, systemImage: "chevron.down")
                    .labelStyle(.trailingIcon)
            }
            .menuStyle(.button)
            .buttonStyle(.huddle(size: .small))
            .menuIndicator(.hidden)
            .menuOrder(.fixed)
            .accessibilityLabel(Text("Duration"))
            .accessibilityValue(Text(form.duration.label))
        }
        if form.duration == .custom {
            HuddleDivider()
            EventEditorRow("Ends") {
                if let end = Binding($form.customEnd) {
                    DatePicker("Ends", selection: end, displayedComponents: [.date, .hourAndMinute])
                        .labelsHidden()
                        .datePickerStyle(.compact)
                } else {
                    Button("Set end time") { form.addCustomEnd() }
                        .buttonStyle(.huddle(size: .small))
                }
            }
        }
    }
}
