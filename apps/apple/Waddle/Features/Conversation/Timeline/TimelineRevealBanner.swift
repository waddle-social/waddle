import SwiftUI

/// Floating status over the timeline while a search hit or pin that is not
/// loaded yet is paged in, and the notice when it could not be reached.
struct TimelineRevealBanner: View {
    enum Phase: Equatable {
        case finding
        case missed
    }

    let phase: Phase

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            switch phase {
            case .finding:
                ProgressView()
                    .controlSize(.small)
                Text("Finding message…")
            case .missed:
                Image(systemName: "exclamationmark.magnifyingglass")
                Text("Couldn't find that message")
            }
        }
        .font(.footnote.weight(.semibold))
        .foregroundStyle(.secondary)
        .padding(.horizontal, Theme.Spacing.m + 2)
        .padding(.vertical, Theme.Spacing.s)
        .waddleGlass(in: Capsule())
        .shadow(color: Color.black.opacity(0.12), radius: 6, y: 2)
        .padding(.top, Theme.Spacing.m)
        .accessibilityElement(children: .combine)
    }
}
