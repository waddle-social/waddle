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
                .huddleText(Theme.Typography.caption.weight(600))
                .foregroundStyle(Theme.Colors.dangerText)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.m)
        } else if state.hasLoadedLatest, !state.hasMoreOlder {
            beginning
        } else if state.hasLoadedLatest {
            Button("Load earlier messages", action: onLoadOlder)
                .buttonStyle(.borderless)
                .huddleText(Theme.Typography.caption.weight(600))
                .foregroundStyle(Theme.Colors.actionText)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.m)
        } else {
            Color.clear.frame(height: 1)
        }
    }

    private var beginning: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Text(header.beginningTitle)
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
            Text(header.emptyMessage)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .padding(.horizontal, Theme.Spacing.xl)
        .padding(.top, Theme.Spacing.xl)
        .padding(.bottom, Theme.Spacing.m)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

/// "Today", "Yesterday", weekday or date between days (web `wd-divider`).
struct TimelineDaySeparator: View {
    let day: Date

    var body: some View {
        TimelineDivider(rule: Theme.Colors.borderSoft) {
            Text(TimelineDayLabel.title(for: day))
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

/// Marks where the messages that were unread when the screen opened
/// begin: above the oldest of them in the chat order, below it in the
/// social order. Gold, as web's new-messages divider.
struct TimelineUnreadDivider: View {
    /// Which side of the divider the unread rows are on.
    enum Edge: Hashable {
        case unreadBelow
        case unreadAbove
    }

    let edge: Edge

    var body: some View {
        TimelineDivider(rule: Theme.Colors.gold) {
            Text("New messages")
                .huddleText(Theme.Typography.caption.weight(700))
                .foregroundStyle(Theme.Colors.goldText)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(edge == .unreadAbove ? "New messages above" : "New messages below"))
    }
}

/// A label centred between two hairlines.
private struct TimelineDivider<Label: View>: View {
    let rule: Color
    @ViewBuilder let label: Label

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            line
            label
                .fixedSize()
            line
        }
        .padding(.horizontal, Theme.Spacing.xl)
        .padding(.vertical, Theme.Spacing.s)
    }

    private var line: some View {
        rule.frame(height: WaddleTokens.BorderWidth.hair)
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
                .huddleText(Theme.Typography.button)
                .foregroundStyle(Theme.Colors.actionText)
                .padding(.horizontal, Theme.Spacing.m)
                .frame(minHeight: WaddleTokens.ControlHeight.md)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .huddleSurface(.raised, in: Capsule())
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
