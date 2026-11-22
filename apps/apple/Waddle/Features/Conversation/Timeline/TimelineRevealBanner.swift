import SwiftUI

/// Floating status over the timeline while a search hit or pin that is not
/// loaded yet is paged in, and the notice when it could not be reached.
struct TimelineRevealBanner: View {
    enum Phase: Equatable {
        case finding
        case missed
    }

    let phase: Phase
    /// The timeline edge the banner floats at.
    var edge: VerticalEdge = .top

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
        .huddleText(Theme.Typography.caption.weight(700))
        .foregroundStyle(Theme.Colors.fgMuted)
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.vertical, Theme.Spacing.s)
        .huddleSurface(.raised, in: Capsule())
        .padding(edge == .top ? .top : .bottom, Theme.Spacing.m)
        .accessibilityElement(children: .combine)
    }
}
