import SwiftUI

/// Chat or social timeline, picked from two drawn previews the way
/// Appearance is picked.
struct SettingsMessageOrderPicker: View {
    @Binding var selection: Preferences.MessageOrder

    var body: some View {
        SettingsTilePicker(
            selection: $selection,
            options: Preferences.MessageOrder.allCases,
            label: "Message order",
            title: { "\($0.title) · \($0.subtitle)" }
        ) { order in
            MessageOrderPreview(order: order)
        }
    }
}

/// A miniature conversation: three message rows and the composer, the
/// newest row next to the composer in the action colour.
private struct MessageOrderPreview: View {
    let order: Preferences.MessageOrder

    /// Oldest first, as fractions of the row.
    private let widths: [CGFloat] = [0.55, 0.4, 0.7]

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
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
        .padding(Theme.Spacing.s)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.Colors.bg)
        .accessibilityHidden(true)
    }

    private var composer: some View {
        Capsule()
            .strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
            .background(Capsule().fill(Theme.Colors.surface))
            .frame(height: Theme.Spacing.m)
    }

    @ViewBuilder
    private func rows(newestFirst: Bool) -> some View {
        let ages = newestFirst ? Array(widths.indices.reversed()) : Array(widths.indices)
        ForEach(ages, id: \.self) { age in
            let isNewest = age == widths.count - 1
            GeometryReader { proxy in
                HStack(spacing: Theme.Spacing.xxs) {
                    Circle()
                        .fill(isNewest ? Theme.Colors.action : Theme.Colors.surface2)
                        .frame(width: Theme.Spacing.s, height: Theme.Spacing.s)
                    Capsule()
                        .fill(isNewest ? Theme.Colors.action : Theme.Colors.surface2)
                        .frame(width: proxy.size.width * widths[age], height: Theme.Spacing.xs)
                }
            }
            .frame(height: Theme.Spacing.s)
        }
    }
}
