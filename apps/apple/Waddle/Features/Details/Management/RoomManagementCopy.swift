import Foundation
import WaddleKit

/// Titles, symbols and confirmation copy for the room and group chat
/// actions on the details screen.
extension RoomManagementAction {
    var title: String {
        switch self {
        case .editSettings: return "Edit room"
        case .delete: return "Delete room"
        case .renameGroup: return "Rename group chat"
        case .inviteToGroup: return "Add people"
        case .leaveGroup: return "Leave group chat"
        }
    }

    var symbol: String {
        switch self {
        case .editSettings, .renameGroup: return "pencil"
        case .delete: return "trash"
        case .inviteToGroup: return "person.badge.plus"
        case .leaveGroup: return "rectangle.portrait.and.arrow.right"
        }
    }

    /// Delete and leave end with the conversation gone, so they ask first
    /// and sit apart from the other actions.
    var isDestructive: Bool {
        switch self {
        case .delete, .leaveGroup: return true
        case .editSettings, .renameGroup, .inviteToGroup: return false
        }
    }
}

/// What a confirmation for delete or leave says.
struct RoomExitConfirmation: Equatable {
    let action: RoomManagementAction
    let title: String
    let message: String
    let confirmTitle: String
    let failureTitle: String

    /// `leavesHuddle`: we are in the room's huddle, which is left first.
    init?(_ action: RoomManagementAction, name: String, leavesHuddle: Bool = false) {
        switch action {
        case .delete:
            title = "Delete “\(name)”?"
            message = leavesHuddle
                ? "The room and all its messages are removed for everyone, and you leave the huddle. This can't be undone."
                : "The room and all its messages are removed for everyone. This can't be undone."
            failureTitle = "Couldn't delete the room"
        case .leaveGroup:
            title = "Leave “\(name)”?"
            message = (leavesHuddle ? "You'll leave the huddle and stop getting its messages." : "You'll stop getting its messages.")
                + " To come back, someone in the group chat needs to add you again."
            failureTitle = "Couldn't leave the group chat"
        case .editSettings, .renameGroup, .inviteToGroup:
            return nil
        }
        self.action = action
        confirmTitle = action.title
    }
}
