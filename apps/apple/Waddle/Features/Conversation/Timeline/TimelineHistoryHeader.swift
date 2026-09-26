import SwiftUI
import WaddleKit

/// Top of the feed: loads the next XEP-0313 page when it scrolls into view,
/// shows progress while loading, and marks the beginning of history.
struct TimelineHistoryHeader: View {
    let state: HistoryState
    let header: ConversationHeaderText
    let onLoadOlder: () -> Void

    var body: some View {
        // One stable container, so changing content does not re-fire
        // onAppear and chain page loads while the header stays laid out.
        VStack(alignment: .leading, spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .onAppear(perform: onLoadOlder)
    }

    @ViewBuilder
    private var content: some View {
        if state.isLoading {
            ProgressView()
                .controlSize(.small)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.m)
                .accessibilityLabel(Text("Loading earlier messages"))
        } else if state.failed {
            Button("Couldn't load earlier messages. Try again", action: onLoadOlder)
                .buttonStyle(.borderless)
                .font(.footnote)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.m)
        } else if state.hasLoadedLatest, !state.hasMoreOlder {
            beginning
        } else if state.hasLoadedLatest {
            Button("Load earlier messages", action: onLoadOlder)
                .buttonStyle(.borderless)
                .font(.footnote)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.m)
        } else {
            Color.clear.frame(height: 1)
        }
    }

    private var beginning: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Text(header.beginningTitle)
                .font(.title3.weight(.bold))
            Text(header.emptyMessage)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.top, Theme.Spacing.xl)
        .padding(.bottom, Theme.Spacing.m)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

/// "Today", "Yesterday", weekday or date between days.
struct TimelineDaySeparator: View {
    let day: Date

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            rule
            Text(TimelineDayLabel.title(for: day))
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .fixedSize()
            rule
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.s)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }

    private var rule: some View {
        Rectangle()
            .fill(Color.secondary.opacity(0.2))
            .frame(height: 1)
    }
}

/// Marks where the messages that were unread when the screen opened
/// begin: above the oldest of them in the chat order, below it in the
/// social order.
struct TimelineUnreadDivider: View {
    /// Which side of the divider the unread rows are on.
    enum Edge: Hashable {
        case unreadBelow
        case unreadAbove
    }

    let edge: Edge

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Rectangle()
                .fill(Color.red.opacity(0.7))
                .frame(height: 1)
            Label("Unread", systemImage: edge == .unreadAbove ? "arrow.up" : "arrow.down")
                .labelStyle(.titleAndIcon)
                .font(.caption2.weight(.bold))
                .textCase(.uppercase)
                .foregroundStyle(Color.red)
                .fixedSize()
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.xs)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(edge == .unreadAbove ? "Unread messages above" : "Unread messages below"))
    }
}

/// Floating button back to the newest message, at the feed's newest
/// edge.
struct JumpToLatestPill: View {
    let unseenCount: Int
    var edge: VerticalEdge = .bottom
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: edge == .top ? "arrow.up" : "arrow.down")
                .contentTransition(.numericText(value: Double(unseenCount)))
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Color.accentColor)
                .padding(.horizontal, Theme.Spacing.m + 2)
                .padding(.vertical, Theme.Spacing.s)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .waddleInteractiveGlass(in: Capsule())
        .shadow(color: Color.black.opacity(0.12), radius: 6, y: 2)
        .padding(edge == .top ? .top : .bottom, Theme.Spacing.m)
        .accessibilityHint(Text("Scrolls to the newest message"))
    }

    private var title: String {
        switch unseenCount {
        case 0: return "Jump to latest"
        case 1: return "1 new message"
        default: return "\(unseenCount) new messages"
        }
    }
}
