import SwiftUI

/// GIPHY attribution, required wherever its results show, with a spinner
/// while a new search runs over the current results.
struct GifPickerFooter: View {
    let isSearching: Bool

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            if isSearching {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(Text("Searching"))
            }
            Text("Powered by GIPHY")
                .huddleText(Theme.Typography.caption.weight(700))
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.s)
        .overlay(alignment: .top) {
            HuddleDivider()
        }
    }
}
