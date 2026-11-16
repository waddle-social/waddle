import SwiftUI

/// The shape every navigation row shares: one line, a rounded hit area,
/// a soft tint on hover, and a tint plus the keel when selected.
struct NavigationRowChrome<Label: View>: View {
    var isSelected = false
    @ViewBuilder let label: Label
    @State private var isHovered = false

    var body: some View {
        label
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Theme.Spacing.m)
            .frame(minHeight: Self.height)
            .background(
                RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous)
                    .fill(Theme.Colors.fg.opacity(tint))
            )
            .overlay(alignment: .leading) {
                if isSelected {
                    Capsule()
                        .fill(Theme.Colors.action)
                        .frame(width: WaddleTokens.BorderWidth.focus, height: Self.height / 2)
                        .transition(.opacity)
                }
            }
            .contentShape(Rectangle())
            .onHover { isHovered = $0 }
            .huddleAnimation(.press, value: isSelected)
    }

    /// Web `state.selected` and `state.hover`: one tint for any surface.
    private var tint: Double {
        if isSelected { return 0.1 }
        return isHovered ? 0.06 : 0
    }

    /// Touch gets the 44pt target; a pointer gets the 36pt row.
    static var height: CGFloat {
        #if os(iOS)
        WaddleTokens.ControlHeight.touch
        #else
        WaddleTokens.ControlHeight.md
        #endif
    }
}

/// Icon, title and trailing state of a navigation row.
struct NavigationRowLabel<Icon: View, Trailing: View>: View {
    let title: String
    var isUnread = false
    var isDimmed = false
    @ViewBuilder let icon: Icon
    @ViewBuilder let trailing: Trailing

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            icon
                .frame(width: Self.iconWidth, height: Self.iconWidth)
            NavigationRowTitle(title: title, isUnread: isUnread, isDimmed: isDimmed)
            Spacer(minLength: Theme.Spacing.xs)
            trailing
        }
    }

    static var iconWidth: CGFloat { 20 }
}

/// A row title: bold and full-strength while unread, quiet once read,
/// quieter still when muted.
struct NavigationRowTitle: View {
    let title: String
    var isUnread = false
    var isDimmed = false

    var body: some View {
        Text(title)
            .huddleText(Theme.Typography.sidebar.weight(isUnread ? 700 : 500))
            .foregroundStyle(color)
            .lineLimit(1)
    }

    private var color: Color {
        if isUnread { return Theme.Colors.fg }
        return isDimmed ? Theme.Colors.fgSoft : Theme.Colors.fgMuted
    }
}

extension View {
    /// Lays a navigation row out in a plain list: tight insets, no
    /// separator, and the list's own ground showing through.
    func navigationListRow(top: CGFloat = 1) -> some View {
        listRowInsets(EdgeInsets(top: top, leading: Theme.Spacing.s, bottom: 1, trailing: Theme.Spacing.s))
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }
}
