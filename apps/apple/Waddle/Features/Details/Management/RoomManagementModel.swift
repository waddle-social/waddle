import Foundation
import Observation
import WaddleKit

/// The sheet a room or group chat action opens.
enum RoomManagementSheet: Identifiable, Hashable {
    case editRoom
    case renameGroup
    case addPeople

    var id: Self { self }
}

/// A delete or leave the server refused.
struct RoomExitFailure: Equatable {
    let action: RoomManagementAction
    let message: String
}

/// Room and group chat management on one details screen: which sheet is
/// open, and delete or leave from confirmation to the move away.
@MainActor
@Observable
final class RoomManagementModel {
    let room: BareJID
    var sheet: RoomManagementSheet?
    /// Delete or leave, waiting for confirmation.
    var pendingExit: RoomManagementAction?
    /// Delete or leave, sent and not answered yet.
    private(set) var exiting: RoomManagementAction?
    var exitFailure: RoomExitFailure?

    init(room: BareJID) {
        self.room = room
    }

    func select(_ action: RoomManagementAction) {
        switch action {
        case .editSettings: sheet = .editRoom
        case .renameGroup: sheet = .renameGroup
        case .inviteToGroup: sheet = .addPeople
        case .delete, .leaveGroup: pendingExit = action
        }
    }

    /// Runs a confirmed delete or leave. The room is already gone from the
    /// directory when it succeeds, so navigation moves off it.
    func exit(_ action: RoomManagementAction, using session: SessionCoordinator, navigation: NavigationModel) async {
        guard action.isDestructive, exiting == nil else { return }
        pendingExit = nil
        exiting = action
        defer { exiting = nil }
        do {
            switch action {
            case .delete: try await session.deleteRoom(room)
            case .leaveGroup: try await session.leaveGroupDM(room)
            case .editSettings, .renameGroup, .inviteToGroup: return
            }
            navigation.close(.room(room))
        } catch {
            exitFailure = RoomExitFailure(
                action: action,
                message: ActionErrorCopy.message(for: error, fallback: "That didn't work. Try again.")
            )
        }
    }
}
