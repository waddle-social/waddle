import SwiftUI
import WaddleKit

/// The room directory link and the space groups in the split sidebar.
struct SidebarRoomsSection: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let expansion: SidebarSpaceExpansion
    let searchResults: [Channel]?

    var body: some View {
        if let searchResults {
            ForEach(searchResults) { channel in
                ChannelNavigationRow(channel: channel, style: .sidebar)
            }
        } else {
            groupedRooms
        }
    }

    @ViewBuilder
    private var groupedRooms: some View {
        let groups = SidebarRoomGroups(session: session).groups
        NavigationSectionHeader(
            title: "Rooms",
            addLabel: "Create standalone room",
            action: standaloneCreation(in: groups),
            accessory: { BrowseRoomsSidebarLink() }
        )
        if !session.directory.hasLoadedTopology && groups.isEmpty {
            NavigationLoadingRow(title: "Loading rooms…")
        } else if groups.isEmpty {
            Text("No rooms yet.")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .padding(.leading, Theme.Spacing.m)
                .navigationListRow()
        }
        ForEach(groups) { group in
            SidebarSpaceSection(
                group: group,
                isExpanded: Binding(
                    get: { expansion.isExpanded(group.id, for: session.account.jid) },
                    set: { expansion.setExpanded($0, group: group.id, for: session.account.jid) }
                ),
                createRoom: creation(in: group)
            )
        }
        if !session.directory.leftRooms.isEmpty {
            NavigationEmptyAction(title: "Rooms you left", symbol: "arrow.uturn.backward") {
                navigation.sheet = .leftRooms
            }
        }
    }

    private func standaloneCreation(in groups: [SidebarRoomGroups.Group]) -> (() -> Void)? {
        guard session.isCommunityOwner, !groups.contains(where: { $0.id == .standalone }) else { return nil }
        return { navigation.sheet = .newChannel }
    }

    private func creation(in group: SidebarRoomGroups.Group) -> (() -> Void)? {
        guard session.isCommunityOwner else { return nil }
        if let space = group.space {
            guard space.serviceJID != nil else { return nil }
            return { navigation.sheet = .newChannelInSpace(space) }
        }
        return { navigation.sheet = .newChannel }
    }
}

/// Reveals a room selected through search or another screen, including when
/// the directory discovers it after selection. Search can hide the grouped rows.
struct RevealSidebarRoom: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let expansion: SidebarSpaceExpansion

    private struct Selection: Equatable {
        let conversation: ConversationID
        let group: SidebarRoomGroups.Group.ID
    }

    private var selection: Selection? {
        guard let conversation = navigation.selection,
            let group = SidebarRoomGroups.groupID(for: conversation, in: session.directory)
        else { return nil }
        return Selection(conversation: conversation, group: group)
    }

    func body(content: Content) -> some View {
        content.onChange(of: selection) { _, selected in
            guard let selected else { return }
            expansion.setExpanded(true, group: selected.group, for: session.account.jid)
        }
    }
}
