import SwiftUI

/// A section label in a navigation list, with a trailing "+".
/// It is an ordinary row, so it scrolls with its section and never floats.
struct NavigationSectionHeader<Accessory: View>: View {
    let title: String
    let addLabel: String
    let action: () -> Void
    @ViewBuilder let accessory: () -> Accessory

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Text(title)
                .huddleText(Theme.Typography.sidebarSection)
                .foregroundStyle(Theme.Colors.fgSoft)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 0)
            accessory()
            Button(action: action) {
                NavigationIconLabel(symbol: "plus", color: Theme.Colors.fgSoft)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text(addLabel))
            .help(addLabel)
        }
        .padding(.leading, Theme.Spacing.m)
        .navigationListRow(top: Theme.Spacing.m)
    }
}

extension NavigationSectionHeader where Accessory == EmptyView {
    init(title: String, addLabel: String, action: @escaping () -> Void) {
        self.init(title: title, addLabel: addLabel, action: action) { EmptyView() }
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

/// Shown when a navigation search finds nothing.
struct NoMatchesView: View {
    let query: String

    var body: some View {
        EmptyStateView(
            title: "No matches",
            message: "Nothing here is called “\(query)”. Check the spelling or try another name.",
            symbol: "magnifyingglass"
        )
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
