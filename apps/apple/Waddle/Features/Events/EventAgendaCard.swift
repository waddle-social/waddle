import SwiftUI
import WaddleKit

/// An agenda entry with separate actions to open the event and to reply.
struct EventAgendaCard<Header: View>: View {
    @Environment(SessionCoordinator.self) private var session
    let eventID: String
    let onReplyError: (String) -> Void
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
                            choices(rsvp)
                            counts(rsvp)
                        }
                        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                            choices(rsvp)
                            counts(rsvp)
                        }
                    }
                }
                .padding(.horizontal, Theme.Spacing.l)
                .padding(.bottom, Theme.Spacing.m)
            }
        }
    }

    private func choices(_ rsvp: EventRSVP) -> some View {
        HuddleSegmentedControl(
            selection: Binding(get: { rsvp.selection }, set: sendReply),
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

    private func sendReply(_ reply: EventReply?) {
        guard let reply else { return }
        Task {
            do {
                try await session.reply(reply, to: eventID)
            } catch {
                onReplyError(ActionErrorCopy.message(for: error, fallback: "Try again."))
            }
        }
    }
}
