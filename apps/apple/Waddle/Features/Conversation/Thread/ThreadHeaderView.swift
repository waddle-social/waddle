import SwiftUI
import WaddleKit

/// The thread screen's header (web `ThreadPanelHeader`). Top row: the
/// thread's first message, or for a sub-thread the breadcrumb of threads
/// above it, after a back button where the navigation bar does not
/// already go to the parent; a close button in the inspector. Under it:
/// "Started by <name> · N replies · ● active <when>" and the faces.
struct ThreadHeaderView: View {
    let header: ThreadHeader
    /// A fixed clock, for snapshots; the app follows the real one.
    var clock: Date?
    let showsBack: Bool
    /// Opens a thread of the breadcrumb.
    let open: (String) -> Void
    /// Closes the inspector; nil where the navigation bar goes back.
    let close: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            HStack(spacing: Theme.Spacing.xs) {
                if showsBack, let parent = header.parent {
                    HuddleIconButton("Go back", symbol: "chevron.left") { open(parent.threadID) }
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
                if header.isSubThread {
                    breadcrumb
                } else {
                    Text(ThreadHeaderText.title(header.title))
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
                    meta(now: clock ?? context.date)
                }
                Spacer(minLength: 0)
                if !header.faces.isEmpty {
                    MessageThreadFaces(faces: header.faces, overflow: header.otherParticipantCount, size: 22)
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
                    .huddleText(Theme.Typography.tag)
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
                    .help(ThreadHeaderText.title(crumb.title))
                }
            }
        }
    }

    private func crumbLabel(_ crumb: ThreadCrumb, isCurrent: Bool) -> some View {
        Text(ThreadHeaderText.title(crumb.title))
            .huddleText(isCurrent ? Theme.Typography.author : Theme.Typography.caption)
            .foregroundStyle(isCurrent ? Theme.Colors.fg : Theme.Colors.fgMuted)
            .lineLimit(1)
            .truncationMode(.tail)
            .frame(maxWidth: isCurrent ? nil : 160, alignment: .leading)
            .accessibilityLabel(Text("Thread: \(ThreadHeaderText.title(crumb.title))"))
    }

    /// The parts joined with " · ", the recency after a live dot.
    private func meta(now: Date) -> some View {
        let parts = ThreadHeaderText.parts(header, now: now)
        var line = Text(parts.lead.joined(separator: " · "))
        if let activity = parts.activity {
            let dot = Text(Image(systemName: "circle.fill"))
                .font(.system(size: 7))
                .foregroundStyle(Theme.Colors.live)
            line = line + Text(parts.lead.isEmpty ? "" : " · ") + dot + Text(" \(activity)")
        }
        return line
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.fgMuted)
            .lineLimit(1)
            .accessibilityLabel(Text((parts.lead + [parts.activity].compactMap { $0 }).joined(separator: ", ")))
    }
}

/// The header's words.
enum ThreadHeaderText {
    static func title(_ snippet: ThreadSummary.Snippet?) -> String {
        let text = snippet.map(MessageThreadCardText.snippet) ?? ""
        return text.isEmpty ? "Thread" : text
    }

    /// "Started by Priya", "4 replies", and "active 13 min. ago" apart, so
    /// a missing part never leaves a separator behind.
    static func parts(_ header: ThreadHeader, now: Date) -> (lead: [String], activity: String?) {
        var lead: [String] = []
        if let author = header.startedBy {
            lead.append("Started by \(author)")
        }
        lead.append(MessageThreadCardText.count(header.replyCount))
        let activity = header.lastActivity.map { "active \(MessageThreadCardText.time($0, relativeTo: now))" }
        return (lead, activity)
    }
}
