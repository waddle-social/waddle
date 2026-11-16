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
    var isEmphasised = false
    var isDimmed = false
    @ViewBuilder let icon: Icon
    @ViewBuilder let trailing: Trailing

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            icon
                .frame(width: Self.iconWidth, height: Self.iconWidth)
            NavigationRowTitle(title: title, isEmphasised: isEmphasised, isDimmed: isDimmed)
            Spacer(minLength: Theme.Spacing.xs)
            trailing
        }
    }

    static var iconWidth: CGFloat { 20 }
}

/// A row title: bold and full-strength when emphasised (unread, or the
/// current place), quiet otherwise, quieter still when dimmed.
struct NavigationRowTitle: View {
    let title: String
    var isEmphasised = false
    var isDimmed = false

    var body: some View {
        Text(title)
            .huddleText(Theme.Typography.sidebar.weight(isEmphasised ? 700 : 500))
            .foregroundStyle(color)
            .lineLimit(1)
    }

    private var color: Color {
        if isEmphasised { return Theme.Colors.fg }
        return isDimmed ? Theme.Colors.fgSoft : Theme.Colors.fgMuted
    }
}

/// The label of a small icon-only button in navigation chrome. Touch gets
/// the 44pt target; a pointer gets a 32pt one.
struct NavigationIconLabel: View {
    let symbol: String
    var color: Color = Theme.Colors.fgMuted

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: 15, weight: .medium))
            .foregroundStyle(color)
            .frame(width: Self.target, height: Self.target)
            .contentShape(Rectangle())
    }

    static var target: CGFloat {
        #if os(iOS)
        WaddleTokens.ControlHeight.touch
        #else
        WaddleTokens.ControlHeight.sm
        #endif
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
