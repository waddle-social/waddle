import SwiftUI
import WaddleKit

/// Someone in the room now, with their role and a way to remove them.
struct AdminRoomOccupantRow: View {
    let occupant: AdminRoomOccupant
    let choices: [RoomAffiliation]
    let isChanging: Bool
    let choose: (RoomAffiliation) -> Void
    let removeRole: () -> Void
    let remove: () -> Void

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: Theme.Spacing.s) {
                name
                Spacer(minLength: Theme.Spacing.xs)
                controls
            }
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                name
                controls
            }
        }
        .frame(minHeight: Theme.Size.formRow)
    }

    private var name: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            Text(occupant.nick)
                .huddleText(Theme.Typography.author)
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(1)
            Text(occupant.jid.description)
                .huddleText(Theme.Typography.code.sized(WaddleTokens.FontSize.xs))
                .foregroundStyle(Theme.Colors.fgMuted)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(occupant.nick), \(occupant.jid.description)"))
    }

    private var controls: some View {
        HStack(spacing: Theme.Spacing.s) {
            AdminRoleMenu(
                role: occupant.affiliation,
                choices: choices,
                title: AdminRoomCopy.title,
                removeTitle: occupant.affiliation == .none ? nil : "Remove role",
                isChanging: isChanging,
                choose: choose,
                remove: removeRole
            )
            .accessibilityLabel(Text("Role of \(occupant.nick)"))
            Button("Remove", role: .destructive, action: remove)
                .buttonStyle(.huddle(.danger, size: .small))
                .disabled(isChanging)
                .accessibilityLabel(Text("Remove \(occupant.nick) from the room"))
        }
    }
}
