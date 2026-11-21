import SwiftUI
import WaddleKit

/// How the console words a space.
enum AdminSpaceCopy {
    /// "3 rooms · 12 members".
    static func counts(rooms: Int, members: Int) -> String {
        "\(rooms) \(rooms == 1 ? "room" : "rooms") · \(members) \(members == 1 ? "member" : "members")"
    }

    /// What deleting a space takes with it. The server deletes the rooms
    /// that belong to the space and only unlinks the others it lists, so
    /// no number is promised.
    static func deleteWarning(name: String, rooms: Int) -> String {
        rooms == 0
            ? "This deletes \(name). This can't be undone."
            : "This deletes \(name) and the rooms that belong to it. This can't be undone."
    }

    /// Why giving someone a role failed. The server refuses owner and
    /// admin to accounts outside this community and its trusted servers.
    static func roleFailure(_ error: Error, role: AdminSpaceRole, address: String) -> String {
        if error as? PortError == .rejected, role == .owner || role == .admin {
            return "\(address) can't be an owner or admin. Only people with an account here or on a trusted server can."
        }
        return ActionErrorCopy.message(for: error, fallback: "Couldn't change the role of \(address). Try again.")
    }

    static func title(_ role: AdminSpaceRole) -> String {
        switch role {
        case .owner: "Owner"
        case .admin: "Admin"
        case .member: "Member"
        case .none: "No role"
        }
    }
}

/// Name, description and how much is in the space.
struct AdminSpaceRow: View {
    let space: AdminSpace
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            NavigationRowChrome {
                HStack(spacing: Theme.Spacing.m) {
                    VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                        NavigationRowTitle(title: space.name, isEmphasised: true)
                        if let description = space.description {
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
        .accessibilityLabel(Text([space.name, space.description, counts].compactMap { $0 }.joined(separator: ", ")))
        .accessibilityAddTraits(.isButton)
        .accessibilityHint(Text("Opens the space"))
    }

    private var counts: String {
        AdminSpaceCopy.counts(rooms: space.roomCount, members: space.memberCount)
    }
}
