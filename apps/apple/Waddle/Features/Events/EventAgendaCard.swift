import SwiftUI
import WaddleKit

/// An agenda entry with separate actions to open the event and to reply.
struct EventAgendaCard<Header: View>: View {
    @Environment(SessionCoordinator.self) private var session
    let eventID: String
    var insetsFooter = true
    let replyAction: EventReplyAction
    @ViewBuilder let header: Header

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if let event = session.events.event(eventID) {
                let rsvp = EventRSVP(event: event, viewer: session.account.jid)
                VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                    HuddleDivider()
                    ViewThatFits(in: .horizontal) {
                        HStack(spacing: Theme.Spacing.m) {
                            choices
                            counts(rsvp)
                        }
                        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                            choices
                            counts(rsvp)
                        }
                    }
                }
                .padding(.horizontal, insetsFooter ? Theme.Spacing.l : 0)
                .padding(.bottom, insetsFooter ? Theme.Spacing.m : 0)
            }
        }
    }

    private var choices: some View {
        HuddleSegmentedControl(
            selection: replyAction.selection(for: eventID, in: session),
            segments: EventReply.allCases.map { .init(value: $0, title: EventRSVP.choiceLabel(for: $0)) }
        )
        .accessibilityLabel(Text("RSVP"))
    }

    private func counts(_ rsvp: EventRSVP) -> some View {
        Text(rsvp.countsLabel)
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.fgMuted)
            .monospacedDigit()
    }
}
