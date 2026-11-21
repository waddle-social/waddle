import Testing
import WaddleKit

@Suite("Room management permissions")
struct RoomManagementPermissionsTests {
    @Test func missingChannelOffersNothing() {
        #expect(RoomManagementPermissions.actions(for: nil, occupant: nil) == [])
    }
    @Test func normalRoomRequiresExactlyOwnerAffiliation() {
        let channel = Channel(roomJID: BareJID(localpart: "general", domain: "muc.waddle.test")!, name: "General")
        let rows: [(RoomAffiliation?, [RoomManagementAction])] = [
            (.owner, [.editSettings, .delete]), (.admin, []), (.member, []),
            (RoomAffiliation.none, []), (.outcast, []), (nil, []),
        ]
        for (affiliation, expected) in rows {
            #expect(RoomManagementPermissions.actions(for: channel, occupant: occupant(affiliation)) == expected)
        }
    }

    @Test func groupMembersCanRenameAndInviteAndEveryoneCanLeave() {
        let channel = Channel(roomJID: BareJID(localpart: "friends", domain: "muc.waddle.test")!, name: "Friends", isGroupDM: true)
        let rows: [(RoomAffiliation?, [RoomManagementAction])] = [
            (.owner, [.renameGroup, .inviteToGroup, .leaveGroup]),
            (.admin, [.renameGroup, .inviteToGroup, .leaveGroup]),
            (.member, [.renameGroup, .inviteToGroup, .leaveGroup]),
            (RoomAffiliation.none, [.leaveGroup]), (.outcast, [.leaveGroup]), (nil, [.leaveGroup]),
        ]
        for (affiliation, expected) in rows {
            #expect(RoomManagementPermissions.actions(for: channel, occupant: occupant(affiliation)) == expected)
        }
    }

    private func occupant(_ affiliation: RoomAffiliation?) -> Occupant? {
        affiliation.map {
            Occupant(nick: "alice", availability: .available, status: nil, affiliation: $0,
                     role: .moderator, realJID: nil, hats: [])
        }
    }
}
