import SwiftUI

/// A selection menu with a Huddle button for the current value.
struct HuddlePicker<Value: Hashable, Options: View>: View {
    let title: String
    let valueTitle: String
    @Binding var selection: Value
    @ViewBuilder let options: Options

    var body: some View {
        Menu {
            Picker(title, selection: $selection) {
                options
            }
            .pickerStyle(.inline)
        } label: {
            Label(valueTitle, systemImage: "chevron.down")
                .labelStyle(.trailingIcon)
        }
        .menuStyle(.button)
        .buttonStyle(.huddle(size: .small))
        .menuIndicator(.hidden)
        .accessibilityLabel(Text(title))
        .accessibilityValue(Text(valueTitle))
    }
}
