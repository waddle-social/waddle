import SwiftUI

/// The console's caption and section switch share the list's leading edge.
struct AdminConsoleHeader: View {
    @Binding var selection: AdminSection
    let caption: String
    var spaceFilter: AdminSpaceFilter?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            Text(caption)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
            if let spaceFilter {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Theme.Spacing.s) {
                        sections
                        Spacer(minLength: 0)
                        spaceFilter
                    }
                    VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                        sections
                        spaceFilter.frame(maxWidth: .infinity, alignment: .trailing)
                    }
                }
            } else {
                sections
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Theme.Spacing.l)
        .frame(maxWidth: AdminConsoleLayout.readableWidth)
        .frame(maxWidth: .infinity)
    }

    private var sections: some View {
        HuddleSegmentedControl(
            selection: $selection,
            segments: AdminSection.allCases.map { .init(value: $0, title: $0.copy.title) }
        )
        .accessibilityLabel(Text("Admin sections"))
    }
}

enum AdminConsoleLayout {
    static let readableWidth: CGFloat = 720
}
