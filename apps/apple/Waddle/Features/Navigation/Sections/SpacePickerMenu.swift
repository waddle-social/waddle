import SwiftUI
import WaddleKit

/// XEP-0503 space scope for the channel list, shown when there is more
/// than one space.
struct SpacePickerMenu: View {
    let spaces: [Space]
    @Binding var selection: String?

    var body: some View {
        Menu {
            Picker("Space", selection: $selection) {
                Text("All spaces").tag(String?.none)
                ForEach(spaces) { space in
                    Text(space.name).tag(Optional(space.id))
                }
            }
            .pickerStyle(.inline)
        } label: {
            HStack(spacing: Theme.Spacing.s) {
                Text(SpaceScope.title(for: SpaceScope.resolved(selection, in: spaces), in: spaces))
                    .huddleText(Theme.Typography.channel)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.Colors.fgSoft)
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .accessibilityLabel(Text("Space"))
        .accessibilityValue(Text(SpaceScope.title(for: SpaceScope.resolved(selection, in: spaces), in: spaces)))
    }
}
