import SwiftUI
import WaddleKit

extension View {
    /// The sheets, confirmations and failure alert behind the room and
    /// group chat actions on a details screen.
    func roomManagementDialogs(
        _ model: RoomManagementModel,
        name: String,
        groupMembers: @escaping () -> Set<BareJID>
    ) -> some View {
        modifier(RoomManagementDialogs(model: model, name: name, groupMembers: groupMembers))
    }
}

private struct RoomManagementDialogs: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Bindable var model: RoomManagementModel
    let name: String
    /// Us and the people known to be in the group chat, read when Add
    /// people opens.
    let groupMembers: () -> Set<BareJID>

    func body(content: Content) -> some View {
        content
            .confirmationDialog(
                confirmation?.title ?? "",
                isPresented: confirmationBinding,
                titleVisibility: .visible,
                presenting: confirmation
            ) { confirmation in
                Button(confirmation.confirmTitle, role: .destructive) {
                    let session = self.session
                    let navigation = self.navigation
                    Task { await model.exit(confirmation.action, using: session, navigation: navigation) }
                }
                Button("Cancel", role: .cancel) {
                    model.pendingExit = nil
                }
            } message: { confirmation in
                Text(confirmation.message)
            }
            .alert(failureTitle, isPresented: failureBinding) {
                Button("OK", role: .cancel) {
                    model.exitFailure = nil
                }
            } message: {
                Text(model.exitFailure?.message ?? "")
            }
            .sheet(item: $model.sheet) { sheet in
                sheetContent(sheet)
                    .carryingCallDock()
                    .environment(session)
                    .environment(navigation)
            }
    }

    @ViewBuilder
    private func sheetContent(_ sheet: RoomManagementSheet) -> some View {
        switch sheet {
        case .editRoom:
            EditRoomSheet(room: model.room)
        case .renameGroup:
            RenameGroupSheet(editor: GroupRenameEditor(
                room: model.room,
                channelName: session.directory.channel(for: model.room)?.name
            ))
        case .addPeople:
            AddPeopleSheet(model: GroupInviteModel(room: model.room, excluded: groupMembers()))
        }
    }

    private var confirmation: RoomExitConfirmation? {
        model.pendingExit.flatMap { RoomExitConfirmation($0, name: name) }
    }

    private var failureTitle: String {
        model.exitFailure.flatMap { RoomExitConfirmation($0.action, name: name)?.failureTitle } ?? ""
    }

    private var confirmationBinding: Binding<Bool> {
        Binding(
            get: { model.pendingExit != nil },
            set: { if !$0 { model.pendingExit = nil } }
        )
    }

    private var failureBinding: Binding<Bool> {
        Binding(
            get: { model.exitFailure != nil },
            set: { if !$0 { model.exitFailure = nil } }
        )
    }
}
