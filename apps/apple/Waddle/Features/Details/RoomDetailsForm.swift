import SwiftUI
import WaddleKit

/// Room details (web `drawer-details`): header, room or group chat
/// actions, notifications, members, pins and search, then delete or leave.
struct RoomDetailsForm: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let room: BareJID
    @State private var model: RoomMembersModel
    @State private var management: RoomManagementModel

    init(room: BareJID) {
        self.room = room
        _model = State(initialValue: RoomMembersModel(room: room))
        _management = State(initialValue: RoomManagementModel(room: room))
    }

    var body: some View {
        let channel = session.directory.channel(for: room)
        let actions = RoomManagementPermissions.actions(for: channel, occupant: session.selfOccupant(in: room))
        Form {
            RoomHeaderSection(room: room, channel: channel)
            RoomManagementSection(actions: actions, onSelect: management.select)
            // Left on another device while this is open: its notifications
            // stay off until it is joined again.
            if channel?.hasLeft != true {
                NotificationModeSection(conversation: .room(room))
            }
            Section {
                NavigationLink {
                    RoomMembersView(room: room, model: model)
                } label: {
                    FormRowLabel(title: "Members", symbol: "person.2") {
                        Text("\(hereCount) here")
                    }
                }
                NavigationLink {
                    PinnedMessagesView(conversation: .room(room))
                } label: {
                    FormRowLabel(title: "Pinned messages", symbol: "pin") {
                        Text("\(session.pins.pins(in: room).count)")
                    }
                }
                FormActionButton(title: "Search in conversation", symbol: "magnifyingglass") {
                    navigation.sheet = .search(.room(room))
                }
            }
            RoomExitSection(actions: actions, exiting: management.exiting, onSelect: management.select)
        }
        .huddleForm()
        .roomManagementDialogs(management, name: session.directory.title(for: .room(room)), groupMembers: groupMembers)
    }

    /// People in the room now, as Members counts them.
    private var hereCount: Int {
        let present = Array((session.presence.occupants[room] ?? [:]).values)
        return MemberRoster.cards(present: present, members: nil, contacts: [:]).count
    }

    /// Us, the people here and, once loaded, every member. Not the banned,
    /// which owners and admins load with the members.
    private func groupMembers() -> Set<BareJID> {
        let present = (session.presence.occupants[room] ?? [:]).values.compactMap(\.realJID)
        return Self.groupMemberJIDs(present: present, members: model.members, account: session.account.jid)
    }

    static func groupMemberJIDs(present: [BareJID], members: [RoomMember]?, account: BareJID) -> Set<BareJID> {
        let memberJIDs = members?.filter { $0.affiliation != .outcast }.map(\.jid) ?? []
        return Set(present + memberJIDs + [account])
    }
}
