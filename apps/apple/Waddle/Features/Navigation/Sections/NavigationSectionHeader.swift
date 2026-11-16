import SwiftUI

/// A section label in a navigation list, with an optional trailing "+".
/// It is an ordinary row, so it scrolls with its section and never floats.
struct NavigationSectionHeader: View {
    let title: String
    var addLabel: String?
    var action: () -> Void = {}

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Text(title)
                .huddleText(Theme.Typography.sidebarSection)
                .foregroundStyle(Theme.Colors.fgSoft)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 0)
            if let addLabel {
                Button(action: action) {
                    Image(systemName: "plus")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.Colors.fgSoft)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text(addLabel))
                .help(addLabel)
            }
        }
        .padding(.leading, Theme.Spacing.m)
        .padding(.trailing, Theme.Spacing.xs)
        .navigationListRow(top: Theme.Spacing.m)
    }
}

/// A quiet in-list progress row.
struct NavigationLoadingRow: View {
    let title: String

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            ProgressView()
                .controlSize(.small)
            Text(title)
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgSoft)
        }
        .padding(.horizontal, Theme.Spacing.m)
        .navigationListRow()
        .accessibilityElement(children: .combine)
    }
}

/// A quiet in-list action for an empty section ("New room", "New message").
struct NavigationEmptyAction: View {
    let title: String
    let symbol: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            NavigationRowChrome {
                NavigationRowLabel(title: title) {
                    Image(systemName: symbol)
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(Theme.Colors.fgSoft)
                } trailing: {
                    EmptyView()
                }
            }
        }
        .buttonStyle(.plain)
        .navigationListRow()
    }
}
