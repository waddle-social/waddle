import SwiftUI

extension View {
    /// A form on the Huddle ground (web `drawer-details`, `settings`):
    /// grouped sections on `bg`, not the system's grey.
    func huddleForm() -> some View {
        formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .background(Theme.Colors.bg)
    }
}

/// A section title in a form (web `eyebrow`).
struct FormSectionHeader: View {
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

/// The note under a form section: caption text, muted.
struct FormSectionFooter: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.fgMuted)
    }
}

/// A switch row in a form: its label, with a glyph when the form's rows
/// have them, and the Huddle switch.
struct FormToggle: View {
    let title: String
    var symbol: String?
    let caption: String?
    @Binding var isOn: Bool

    init(_ title: String, symbol: String? = nil, caption: String? = nil, isOn: Binding<Bool>) {
        self.title = title
        self.symbol = symbol
        self.caption = caption
        _isOn = isOn
    }

    var body: some View {
        Toggle(isOn: $isOn) {
            if let caption {
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    titleLabel
                    FormSectionFooter(caption)
                }
            } else {
                titleLabel
            }
        }
        .toggleStyle(.huddle)
        .frame(minHeight: Theme.Size.formRow)
    }

    @ViewBuilder
    private var titleLabel: some View {
        if let symbol {
            FormRowLabel(title: title, symbol: symbol)
        } else {
            Text(title)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fg)
        }
    }
}

/// A row with a title and a value on the right (`LabeledContent` in
/// Huddle type).
struct FormValueRow: View {
    let title: String
    let value: String

    var body: some View {
        LabeledContent {
            Text(value)
                .foregroundStyle(Theme.Colors.fgMuted)
                .textSelection(.enabled)
        } label: {
            Text(title)
                .foregroundStyle(Theme.Colors.fg)
        }
        .huddleText(Theme.Typography.body)
    }
}

/// An action row in a form: a glyph and a label across the row,
/// in danger text for leave, delete and the like. Plain on every platform:
/// a Mac form otherwise draws each action as a grey push button.
struct FormActionButton<Trailing: View>: View {
    let title: String
    let symbol: String
    var isDestructive = false
    let action: () -> Void
    @ViewBuilder var trailing: Trailing

    var body: some View {
        Button(role: isDestructive ? .destructive : nil, action: action) {
            FormRowLabel(title: title, symbol: symbol, isDestructive: isDestructive) {
                trailing
            }
        }
        .buttonStyle(.plain)
    }
}

extension FormActionButton where Trailing == EmptyView {
    init(title: String, symbol: String, isDestructive: Bool = false, action: @escaping () -> Void) {
        self.init(title: title, symbol: symbol, isDestructive: isDestructive, action: action) { EmptyView() }
    }
}

/// Glyph, label and an optional trailing value, as a form row draws
/// them; also the label of a row that pushes a screen.
struct FormRowLabel<Trailing: View>: View {
    let title: String
    let symbol: String
    var isDestructive = false
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            Image(systemName: symbol)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(isDestructive ? Theme.Colors.dangerText : Theme.Colors.fgMuted)
                .frame(minWidth: Theme.Size.rowGlyph)
                .accessibilityHidden(true)
            Text(title)
                .huddleText(Theme.Typography.label)
                .foregroundStyle(isDestructive ? Theme.Colors.dangerText : Theme.Colors.fg)
            Spacer(minLength: Theme.Spacing.s)
            trailing
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, minHeight: Theme.Size.formRow, alignment: .leading)
        .contentShape(Rectangle())
    }
}

extension FormRowLabel where Trailing == EmptyView {
    init(title: String, symbol: String, isDestructive: Bool = false) {
        self.init(title: title, symbol: symbol, isDestructive: isDestructive) { EmptyView() }
    }
}

extension View {
    /// A text field drawn as a Huddle input (web `input`): the surface, a
    /// strong hairline and the control radius.
    func huddleField() -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        return textFieldStyle(.plain)
            .huddleText(Theme.Typography.body)
            .foregroundStyle(Theme.Colors.fg)
            .padding(.horizontal, Theme.Spacing.m)
            .padding(.vertical, Theme.Spacing.s)
            .frame(minHeight: WaddleTokens.ControlHeight.lg)
            .background(shape.fill(Theme.Colors.surface))
            .overlay(shape.strokeBorder(Theme.Colors.borderStrong, lineWidth: WaddleTokens.BorderWidth.hair))
    }
}

/// Title then icon, as on a disclosure button ("Edit ⌄").
struct TrailingIconLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: Theme.Spacing.xs) {
            configuration.title
            configuration.icon
        }
    }
}

extension LabelStyle where Self == TrailingIconLabelStyle {
    static var trailingIcon: TrailingIconLabelStyle { TrailingIconLabelStyle() }
}
