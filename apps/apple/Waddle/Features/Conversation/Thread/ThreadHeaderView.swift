import SwiftUI
import WaddleKit

extension EnvironmentValues {
    /// The time the header's "active …" counts from, for snapshots; nil
    /// follows the clock.
    @Entry var threadHeaderNow: Date?
}

/// The thread screen's header (web `ThreadPanelHeader`). Top row: the
/// thread's first message, or for a sub-thread a back button and the
/// breadcrumb of threads above it; a close button in the inspector.
/// Under it: "Started by <name> · N replies · active <when>" and the
/// participants' faces.
struct ThreadHeaderView: View {
    let header: ThreadHeader
    /// Opens a thread of the breadcrumb.
    let open: (String) -> Void
    /// Closes the inspector; nil where the navigation bar goes back.
    let close: (() -> Void)?
    @Environment(\.threadHeaderNow) private var fixedNow

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            HStack(spacing: Theme.Spacing.xs) {
                if header.isSubThread {
                    let parent = header.breadcrumb[header.breadcrumb.count - 2]
                    HuddleIconButton("Go back", symbol: "chevron.left") { open(parent.threadID) }
                        .foregroundStyle(Theme.Colors.fgMuted)
                    breadcrumb
                } else {
                    Text(header.title.map(Self.label) ?? "Thread")
                        .huddleText(Theme.Typography.subtitle)
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .accessibilityAddTraits(.isHeader)
                }
                Spacer(minLength: 0)
                if let close {
                    HuddleIconButton("Close thread", symbol: "xmark", action: close)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
            HStack(spacing: Theme.Spacing.m) {
                TimelineView(.everyMinute) { context in
                    meta(now: fixedNow ?? context.date)
                }
                Spacer(minLength: 0)
                if !header.faces.isEmpty {
                    MessageThreadFaces(
                        faces: header.faces,
                        overflow: header.otherParticipantCount,
                        size: 22,
                        ring: Theme.Colors.bg
                    )
                }
            }
        }
        .padding(.leading, Theme.Spacing.l)
        .padding(.trailing, Theme.Spacing.m)
        .padding(.vertical, Theme.Spacing.s)
        .background(.bar)
        .overlay(alignment: .bottom) { HuddleDivider() }
    }

    /// "Threads › first › second", the current thread last and bold.
    private var breadcrumb: some View {
        HStack(spacing: Theme.Spacing.xxs) {
            Text("Threads")
                .huddleText(Theme.Typography.caption.weight(600))
                .foregroundStyle(Theme.Colors.fgMuted)
                .fixedSize()
            ForEach(Array(header.breadcrumb.enumerated()), id: \.element.threadID) { index, crumb in
                let isCurrent = index == header.breadcrumb.count - 1
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Theme.Colors.fgSoft)
                    .accessibilityHidden(true)
                if isCurrent {
                    crumbLabel(crumb, isCurrent: true)
                        .accessibilityAddTraits(.isHeader)
                } else {
                    Button {
                        open(crumb.threadID)
                    } label: {
                        crumbLabel(crumb, isCurrent: false)
                    }
                    .buttonStyle(.plain)
                    .help(crumb.title.map(Self.label) ?? "Thread")
                }
            }
        }
    }

    private func crumbLabel(_ crumb: ThreadCrumb, isCurrent: Bool) -> some View {
        Text(crumb.title.map(Self.label) ?? "Thread")
            .huddleText(isCurrent ? Theme.Typography.author : Theme.Typography.caption)
            .foregroundStyle(isCurrent ? Theme.Colors.fg : Theme.Colors.fgMuted)
            .lineLimit(1)
            .truncationMode(.tail)
            .frame(maxWidth: isCurrent ? nil : 160, alignment: .leading)
            .accessibilityLabel(Text(crumb.title.map { "Thread: \(Self.label($0))" } ?? "Thread"))
    }

    /// "Started by Priya · 4 replies · ● active 12 min ago".
    private func meta(now: Date) -> some View {
        let activity = header.lastActivity.flatMap { ThreadHeader.activity(since: $0, now: now) }
        return HStack(spacing: Theme.Spacing.xs) {
            Text(Self.subtitle(header))
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .lineLimit(1)
            if activity != nil {
                Circle()
                    .fill(Theme.Colors.live)
                    .frame(width: 8, height: 8)
                    .accessibilityHidden(true)
                Text("active \(activity ?? "")")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
    }

    static func subtitle(_ header: ThreadHeader) -> AttributedString {
        var text = AttributedString()
        if let author = header.startedBy {
            text += AttributedString("Started by ")
            var name = AttributedString(author)
            name.foregroundColor = Theme.Colors.fg
            text += name + AttributedString(" · ")
        }
        var count = AttributedString("\(header.replyCount)")
        count.foregroundColor = Theme.Colors.fg
        text += count + AttributedString(header.replyCount == 1 ? " reply" : " replies")
        if header.lastActivity != nil {
            text += AttributedString(" ·")
        }
        return text
    }

    static func label(_ snippet: ThreadSummary.Snippet) -> String {
        let text = MessageThreadCardText.snippet(snippet)
        return text.isEmpty ? "Thread" : text
    }
}
