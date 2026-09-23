import SwiftUI
import WaddleKit

/// Room details: header, notifications, pins, search and members.
struct RoomDetailsForm: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let room: BareJID
    @State private var model: RoomMembersModel

    init(room: BareJID) {
        self.room = room
        _model = State(initialValue: RoomMembersModel(room: room))
    }

    var body: some View {
        Form {
            RoomHeaderSection(room: room, channel: session.directory.channel(for: room))
            NotificationModeSection(conversation: .room(room))
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
        }
        .formStyle(.grouped)
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
