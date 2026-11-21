import SwiftUI
import WaddleKit

/// The rooms found, a page at a time, or why there are none to show.
/// Choosing one opens it.
struct AdminRoomsPanel: View {
    let rooms: AdminList<AdminRoom>
    let open: (AdminRoom) -> Void

    var body: some View {
        AdminListPanel(list: rooms, copy: .rooms) { room in
            AdminRoomRow(room: room) { open(room) }
        }
    }
}

/// How the console words a room.
enum AdminRoomCopy {
    /// "4 here · 9 members". Members are everyone with a role: owners
    /// and admins too.
    static func counts(present: Int, members: Int) -> String {
        "\(present) here · \(members) \(members == 1 ? "member" : "members")"
    }

    static func title(_ kind: AdminRoomKind) -> String {
        switch kind {
        case .text: "Text"
        case .announcement: "Announcements"
        case .forum: "Forum"
        case .other: "Other"
        }
    }

    /// What a row says about a room beyond its name: nothing for a plain
    /// text room anyone can find and join.
    static func tags(for room: AdminRoom) -> [String] {
        var tags: [String] = []
        if room.kind != .text { tags.append(title(room.kind)) }
        if !room.isListed { tags.append("Hidden") }
        if room.isMembersOnly { tags.append("Members only") }
        return tags
    }

    static func title(_ affiliation: RoomAffiliation) -> String {
        switch affiliation {
        case .owner: "Owner"
        case .admin: "Admin"
        case .member: "Member"
        case .none: "No role"
        case .outcast: "Banned"
        }
    }
}

/// Name, what kind of room it is, and how many people it has.
private struct AdminRoomRow: View {
    let room: AdminRoom
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            NavigationRowChrome {
                HStack(spacing: Theme.Spacing.m) {
                    VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                        NavigationRowTitle(title: room.name, isEmphasised: true)
                        if !tags.isEmpty {
                            HStack(spacing: Theme.Spacing.xs) {
                                ForEach(tags, id: \.self) { HuddleTag($0).fixedSize() }
                            }
                        }
                        if let description = room.description {
                            Text(description)
                                .huddleText(Theme.Typography.caption)
                                .foregroundStyle(Theme.Colors.fgMuted)
                                .lineLimit(1)
                        }
                    }
                    Spacer(minLength: Theme.Spacing.xs)
                    Text(counts)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                .padding(.vertical, Theme.Spacing.s)
            }
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(([room.name] + tags + [room.description, counts].compactMap { $0 }).joined(separator: ", ")))
        .accessibilityAddTraits(.isButton)
        .accessibilityHint(Text("Opens the room"))
    }

    private var tags: [String] { AdminRoomCopy.tags(for: room) }

    private var counts: String {
        AdminRoomCopy.counts(present: room.presentCount, members: room.memberCount)
    }
}
