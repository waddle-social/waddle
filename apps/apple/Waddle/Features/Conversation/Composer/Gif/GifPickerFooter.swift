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
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.s)
        .overlay(alignment: .top) {
            Divider()
        }
    }
}
