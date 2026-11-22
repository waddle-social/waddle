import SwiftUI
import WaddleKit

/// Room details: header, room or group chat actions, notifications, pins,
/// search and members, then delete or leave.
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
                    PinnedMessagesView(conversation: .room(room))
                } label: {
                    LabeledContent {
                        Text("\(session.pins.pins(in: room).count)")
                            .monospacedDigit()
                    } label: {
                        Label("Pinned messages", systemImage: "pin")
                    }
                }
                Button {
                    navigation.sheet = .search(.room(room))
                } label: {
                    Label("Search in conversation", systemImage: "magnifyingglass")
                }
            }
            RoomMembersSections(room: room, model: model)
            RoomExitSection(actions: actions, exiting: management.exiting, onSelect: management.select)
        }
        .formStyle(.grouped)
        .roomManagementDialogs(management, name: session.directory.title(for: .room(room)), groupMembers: groupMembers)
        .confirmationDialog(
            "Ban this person?",
            isPresented: banBinding,
            titleVisibility: .visible,
            presenting: model.pendingBan
        ) { _ in
            Button("Ban", role: .destructive) {
                model.confirmBan(using: session)
            }
            Button("Cancel", role: .cancel) {
                model.pendingBan = nil
            }
        } message: { subject in
            Text("\(subject.nick ?? subject.jid?.description ?? "They") will be removed and can't rejoin until the ban is lifted.")
        }
        .alert("Couldn't update member", isPresented: errorBinding) {
            Button("OK", role: .cancel) {
                model.actionError = nil
            }
        } message: {
            Text(model.actionError ?? "")
        }
    }

    /// Us, the people here and, once loaded, every member.
    private func groupMembers() -> Set<BareJID> {
        let present = (session.presence.occupants[room] ?? [:]).values.compactMap(\.realJID)
        let members = model.members?.map(\.jid) ?? []
        return Set(present + members + [session.account.jid])
    }

    private var banBinding: Binding<Bool> {
        Binding(
            get: { model.pendingBan != nil },
            set: { if !$0 { model.pendingBan = nil } }
        )
    }

    private var errorBinding: Binding<Bool> {
        Binding(
            get: { model.actionError != nil },
            set: { if !$0 { model.actionError = nil } }
        )
    }
}
