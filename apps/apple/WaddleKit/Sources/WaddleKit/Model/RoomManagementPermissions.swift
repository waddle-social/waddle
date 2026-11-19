/// Actions offered for a room or group DM, in display order.
public enum RoomManagementAction: Hashable, Sendable {
    case editSettings
    case delete
    case renameGroup
    case inviteToGroup
    case leaveGroup
}

/// Room management actions allowed by the channel and our affiliation.
public enum RoomManagementPermissions {
    /// Returns actions in display order; a missing channel offers none.
    public static func actions(for channel: Channel?, occupant: Occupant?) -> [RoomManagementAction] {
        guard let channel else { return [] }
        if channel.isGroupDM {
            switch occupant?.affiliation {
            case .owner, .admin, .member:
                return [.renameGroup, .inviteToGroup, .leaveGroup]
            case nil, .some(.none), .outcast:
                return [.leaveGroup]
            }
        }
        return occupant?.affiliation == .owner ? [.editSettings, .delete] : []
    }
}
