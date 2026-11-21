import SwiftUI
import WaddleKit

/// Home's way into People on the phone: a row saying how many are here.
struct PeopleHomeRow: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        let here = PeopleList.hereCount(presence: session.presence.contacts, own: session.account.jid)
        NavigationRowChrome {
            NavigationRowLabel(title: "People") {
                Image(systemName: "person.2")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.Colors.fgMuted)
            } trailing: {
                if here > 0 {
                    Text("\(here) here")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgSoft)
                }
            }
        }
        // The link sits behind the row so the list draws no chevron.
        .background {
            NavigationLink(value: Route.people) { EmptyView() }
                .opacity(0)
        }
        .navigationListRow()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(here > 0 ? "People, \(here) here" : "People"))
        .accessibilityAddTraits(.isButton)
    }
}
