import SwiftUI

/// A list section title with a trailing "+" button.
struct NavigationSectionHeader: View {
    let title: String
    let addLabel: String
    let action: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Text(title)
            Spacer(minLength: 0)
            Button(action: action) {
                Image(systemName: "plus")
                    .imageScale(.medium)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .accessibilityLabel(Text(addLabel))
            .help(addLabel)
        }
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
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}
