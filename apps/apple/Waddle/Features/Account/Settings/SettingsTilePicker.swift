import SwiftUI

/// One choice of a few, drawn as preview tiles with a radio, a title and
/// an optional second line (web `settings` Appearance) on iPad and Mac,
/// and as a segmented control on iPhone (Android `settings`).
struct SettingsTilePicker<Value: Hashable & Identifiable, Preview: View>: View {
    struct Option: Identifiable {
        let value: Value
        let title: String
        var subtitle: String?
        var systemImage: String?

        var id: Value.ID { value.id }
    }

    @Binding var selection: Value
    let options: [Option]
    let label: String
    @ViewBuilder let preview: (Value) -> Preview
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        if isCompact {
            HuddleSegmentedControl(selection: $selection, segments: options.map {
                .init(value: $0.value, title: $0.title, systemImage: $0.systemImage)
            })
            .frame(maxWidth: .infinity)
            .accessibilityLabel(Text(label))
        } else {
            tiles
        }
    }

    private var isCompact: Bool {
        #if os(iOS)
        return sizeClass == .compact
        #else
        return false
        #endif
    }

    private var tiles: some View {
        HStack(alignment: .top, spacing: Theme.Spacing.s) {
            ForEach(options) { option in
                tile(option)
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .sensoryFeedback(.selection, trigger: selection)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(label))
    }

    private func tile(_ option: Option) -> some View {
        let isSelected = option.value == selection
        let card = RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
        let well = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        return Button {
            selection = option.value
        } label: {
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                preview(option.value)
                    .frame(height: Theme.Size.settingsPreview)
                    .frame(maxWidth: .infinity)
                    .clipShape(well)
                    .overlay(well.strokeBorder(Theme.Colors.borderSoft, lineWidth: WaddleTokens.BorderWidth.hair))
                HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.xs) {
                    Image(systemName: isSelected ? "largecircle.fill.circle" : "circle")
                        .foregroundStyle(isSelected ? Theme.Colors.action : Theme.Colors.fgSoft)
                        .contentTransition(.symbolEffect(.replace))
                    VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                        Text(option.title)
                            .foregroundStyle(Theme.Colors.fg)
                        if let subtitle = option.subtitle {
                            Text(subtitle)
                                .foregroundStyle(Theme.Colors.fgMuted)
                        }
                    }
                    .fixedSize(horizontal: false, vertical: true)
                }
                .huddleText(Theme.Typography.caption)
            }
            .padding(Theme.Spacing.s)
            .background(card.fill(Theme.Colors.surface))
            .overlay(
                card.strokeBorder(
                    isSelected ? Theme.Colors.action : Theme.Colors.border,
                    lineWidth: isSelected ? WaddleTokens.BorderWidth.strong : WaddleTokens.BorderWidth.hair
                )
            )
            .contentShape(card)
        }
        // Plain, so each tile takes its own taps inside a form row.
        .buttonStyle(.plain)
        .huddleAnimation(.settle, value: isSelected)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text([option.title, option.subtitle].compactMap { $0 }.joined(separator: ", ")))
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : [.isButton])
    }
}
