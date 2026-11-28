import SwiftUI
import WaddleKit

/// Going / Maybe / Can't go for the user, and who else replied.
struct EventReplyCard: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.locale) private var locale
    let event: CommunityEvent
    @State private var replyAction = EventReplyAction(feedback: .inline)

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text("Are you going?")
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fg)
                if event.recurrence != nil {
                    Text("Your answer counts for every date.")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
            HuddleSegmentedControl(
                selection: replyAction.selection(for: event.id, in: session),
                segments: EventReply.allCases.map { .init(value: $0, title: EventsCopy.label(for: $0)) }
            )
            replyAction.inlineFailure
            HuddleDivider()
            replies
        }
        .padding(Theme.Spacing.l)
        .frame(maxWidth: .infinity, alignment: .leading)
        .huddleSurface(.card, radius: Theme.Radius.card)
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    @ViewBuilder
    private var replies: some View {
        let groups = EventReply.allCases.map { ($0, youFirst(event.attendees(replying: $0))) }.filter { !$0.1.isEmpty }
        if groups.isEmpty {
            Text("No one has answered yet.")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
        } else {
            VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                ForEach(groups, id: \.0) { reply, attendees in
                    group(reply, attendees)
                }
            }
        }
    }

    private func group(_ reply: EventReply, _ attendees: [EventAttendee]) -> some View {
        HStack(alignment: .center, spacing: Theme.Spacing.m) {
            HStack(spacing: -8) {
                ForEach(attendees.prefix(Self.faces), id: \.jid) { attendee in
                    JIDAvatar(jid: attendee.jid, size: Theme.Size.rowAvatar)
                        .overlay(Circle().strokeBorder(Theme.Colors.surface, lineWidth: WaddleTokens.BorderWidth.strong))
                }
            }
            .frame(width: Self.facesWidth, alignment: .leading)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(EventsCopy.count(attendees.count, replying: reply))
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fg)
                Text(names(attendees))
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(2)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private func youFirst(_ attendees: [EventAttendee]) -> [EventAttendee] {
        attendees.filter { $0.jid == session.account.jid } + attendees.filter { $0.jid != session.account.jid }
    }

    private func names(_ attendees: [EventAttendee]) -> String {
        attendees
            .map { $0.jid == session.account.jid ? "You" : CommunityCopy.name(of: $0.jid) }
            .formatted(.list(type: .and).locale(locale))
    }

    private static let faces = 3
    /// Three faces, each overlapping the one before by 8pt.
    private static var facesWidth: CGFloat { Theme.Size.rowAvatar * 3 - 16 }
}
