import SwiftUI

/// Chat or social timeline, picked from two drawn previews the way the
/// system picks Light or Dark appearance.
struct SettingsMessageOrderPicker: View {
    @Binding var selection: Preferences.MessageOrder

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Spacing.xl) {
            ForEach(Preferences.MessageOrder.allCases) { order in
                tile(for: order)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.s)
        .sensoryFeedback(.selection, trigger: selection)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Message order"))
    }

    private func tile(for order: Preferences.MessageOrder) -> some View {
        let isSelected = order == selection
        return Button {
            selection = order
        } label: {
            VStack(spacing: Theme.Spacing.s) {
                MessageOrderPreview(order: order)
                    .frame(width: 84, height: 116)
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous)
                            .strokeBorder(
                                isSelected ? Color.accentColor : Color.secondary.opacity(0.3),
                                lineWidth: isSelected ? 2.5 : 1
                            )
                    )
                    .shadow(color: .black.opacity(isSelected ? 0.12 : 0.05), radius: isSelected ? 6 : 2, y: 2)
                VStack(spacing: Theme.Spacing.xxs) {
                    Text(order.title)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                    Text(order.subtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .multilineTextAlignment(.center)
                Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundStyle(isSelected ? Color.accentColor : Color.secondary.opacity(0.6))
                    .contentTransition(.symbolEffect(.replace))
            }
            .frame(maxWidth: .infinity)
            .contentShape(Rectangle())
        }
        // Plain, so each tile takes its own taps inside a Form row.
        .buttonStyle(.plain)
        .animation(.snappy(duration: 0.2), value: isSelected)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(order.title), \(order.subtitle)"))
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : [.isButton])
    }
}

/// A miniature conversation: three message rows and the composer, with
/// the newest row next to the composer in accent.
private struct MessageOrderPreview: View {
    let order: Preferences.MessageOrder

    /// Oldest first.
    private let widths: [CGFloat] = [38, 30, 44]

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            if order.isNewestFirst {
                composer
                rows(newestFirst: true)
                Spacer(minLength: 0)
            } else {
                Spacer(minLength: 0)
                rows(newestFirst: false)
                composer
            }
        }
        .padding(9)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous)
                .fill(Color.primaryBackground)
        )
        .accessibilityHidden(true)
    }

    private var composer: some View {
        Capsule()
            .strokeBorder(Color.secondary.opacity(0.35), lineWidth: 1)
            .background(Capsule().fill(Color.secondary.opacity(0.08)))
            .frame(height: 12)
            .overlay(alignment: order.isNewestFirst ? .topTrailing : .bottomTrailing) {
                Circle()
                    .fill(Color.accentColor)
                    .frame(width: 8, height: 8)
                    .padding(2)
            }
    }

    @ViewBuilder
    private func rows(newestFirst: Bool) -> some View {
        let ages = newestFirst ? Array(widths.indices.reversed()) : Array(widths.indices)
        ForEach(ages, id: \.self) { age in
            let isNewest = age == widths.count - 1
            HStack(spacing: 4) {
                Circle()
                    .fill(isNewest ? Color.accentColor : Color.secondary.opacity(0.35))
                    .frame(width: 9, height: 9)
                RoundedRectangle(cornerRadius: 2.5, style: .continuous)
                    .fill(isNewest ? Color.accentColor.opacity(0.55) : Color.secondary.opacity(0.25))
                    .frame(width: widths[age], height: 6)
            }
            .opacity(isNewest ? 1 : 0.55 + 0.2 * Double(age))
        }
    }
}
