import SwiftUI
import WaddleKit

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
        if !room.access.isListed { tags.append("Hidden") }
        if room.access.isMembersOnly { tags.append("Members only") }
        return tags
    }

    /// Why giving someone a role failed. The server says `not-allowed`
    /// for owner or admin to an account outside this server and its
    /// trusted ones or in the room over a stream that does not carry the
    /// role, and for any role in a room of an enrolled community; and
    /// `conflict` for the room's last owner.
    static func roleFailure(_ error: Error, address: String) -> String {
        switch error as? AdminRoleRefusal {
        case .lastOwner:
            "\(address) is the room's last owner. Make someone else an owner first."
        case .notAllowed:
            "The server doesn't allow this. An owner or admin needs an account here or on a trusted server, and a room that belongs to another community is managed there."
        case nil:
            ActionErrorCopy.message(for: error, fallback: "Couldn't change the role of \(address). Try again.")
        }
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
struct AdminRoomRow: View {
    let room: AdminRoom
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            NavigationRowChrome {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Theme.Spacing.m) {
                        details
                        Spacer(minLength: 0)
                        countLabel
                    }
                    VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                        details
                        countLabel
                    }
                }
                .padding(.vertical, Theme.Spacing.m)
            }
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(([room.name] + tags + [room.description, counts].compactMap { $0 }).joined(separator: ", ")))
        .accessibilityAddTraits(.isButton)
        .accessibilityHint(Text("Opens the room"))
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            Text(room.name)
                .huddleText(Theme.Typography.author)
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(1)
            if !tags.isEmpty {
                HStack(spacing: Theme.Spacing.xs) {
                    ForEach(tags, id: \.self) { HuddleTag($0) }
                }
            }
            if let description = room.description {
                Text(description)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
            }
        }
    }

    private var countLabel: some View {
        Text(counts)
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.fgMuted)
            .monospacedDigit()
    }

    private var tags: [String] { AdminRoomCopy.tags(for: room) }

    private var counts: String {
        AdminRoomCopy.counts(present: room.presentCount, members: room.memberCount)
    }
}
