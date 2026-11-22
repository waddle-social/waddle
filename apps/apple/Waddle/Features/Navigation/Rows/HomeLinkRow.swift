import SwiftUI
import WaddleKit

/// The rows at the top of the phone's Home that open what the split view
/// lists in its sidebar: Calendar, People and Feed, in the sidebar's order.
struct HomeLinkRows: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        let here = PeopleList.hereCount(presence: session.presence.contacts, own: session.account.jid)
        HomeLinkRow(title: "Calendar", symbol: "calendar", route: .calendar)
        HomeLinkRow(
            title: "People",
            symbol: "person.2",
            route: .people,
            detail: here > 0 ? "\(here) here" : nil
        )
        HomeLinkRow(title: "Feed", symbol: "newspaper", route: .feed)
    }
}

/// A row on the phone's Home that pushes `route`: a glyph, a title and an
/// optional detail at the trailing edge.
struct HomeLinkRow: View {
    let title: String
    let symbol: String
    let route: Route
    var detail: String?

    var body: some View {
        NavigationRowChrome {
            NavigationRowLabel(title: title) {
                Image(systemName: symbol)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.Colors.fgMuted)
            } trailing: {
                if let detail {
                    Text(detail)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgSoft)
                }
            }
        }
        // The link sits behind the row so the list draws no chevron.
        .background {
            NavigationLink(value: route) { EmptyView() }
                .opacity(0)
        }
        .navigationListRow()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text([title, detail].compactMap(\.self).joined(separator: ", ")))
        .accessibilityAddTraits(.isButton)
    }
}
