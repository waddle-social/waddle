import SwiftUI

extension View {
    /// A details form on the Huddle ground (web `drawer-details`): grouped
    /// sections on `bg`, not the system's grey.
    func detailsForm() -> some View {
        formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .background(Theme.Colors.bg)
    }
}

/// A section title in a details form (web `eyebrow`).
struct DetailsSectionHeader: View {
    let title: String

    init(_ title: String) {
        self.title = title
    }

    var body: some View {
        Text(title)
            .huddleText(Theme.Typography.eyebrow)
            .foregroundStyle(Theme.Colors.fgMuted)
            .textCase(nil)
    }
}

/// An action row in a details form: a glyph and a label across the row,
/// in danger text for leave, delete and the like. Plain on every platform:
/// a Mac form otherwise draws each action as a grey push button.
struct DetailsActionButton<Trailing: View>: View {
    let title: String
    let symbol: String
    var isDestructive = false
    let action: () -> Void
    @ViewBuilder var trailing: Trailing

    var body: some View {
        Button(role: isDestructive ? .destructive : nil, action: action) {
            DetailsRowLabel(title: title, symbol: symbol, isDestructive: isDestructive) {
                trailing
            }
        }
        .buttonStyle(.plain)
    }
}

extension DetailsActionButton where Trailing == EmptyView {
    init(title: String, symbol: String, isDestructive: Bool = false, action: @escaping () -> Void) {
        self.init(title: title, symbol: symbol, isDestructive: isDestructive, action: action) { EmptyView() }
    }
}

/// Glyph, label and an optional trailing value, as a details row draws
/// them; also the label of a row that pushes a screen.
struct DetailsRowLabel<Trailing: View>: View {
    let title: String
    let symbol: String
    var isDestructive = false
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            Image(systemName: symbol)
                .foregroundStyle(isDestructive ? Theme.Colors.dangerText : Theme.Colors.fgMuted)
                .frame(width: Theme.Size.rowGlyph)
                .accessibilityHidden(true)
            Text(title)
                .huddleText(Theme.Typography.label)
                .foregroundStyle(isDestructive ? Theme.Colors.dangerText : Theme.Colors.fg)
            Spacer(minLength: Theme.Spacing.s)
            trailing
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .frame(maxWidth: .infinity, minHeight: Theme.Size.detailsRow, alignment: .leading)
        .contentShape(Rectangle())
    }
}

extension DetailsRowLabel where Trailing == EmptyView {
    init(title: String, symbol: String, isDestructive: Bool = false) {
        self.init(title: title, symbol: symbol, isDestructive: isDestructive) { EmptyView() }
    }
}
