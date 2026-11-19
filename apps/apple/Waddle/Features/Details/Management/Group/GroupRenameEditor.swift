import Foundation
import Observation
import WaddleKit

/// The rename sheet's draft. An empty name removes the group chat's name.
@MainActor
@Observable
final class GroupRenameEditor {
    let room: BareJID
    /// The name the group chat has now; empty when it has none.
    let currentName: String
    var name: String
    private(set) var isSaving = false
    private(set) var saveError: String?

    init(room: BareJID, channelName: String?) {
        self.room = room
        currentName = Self.currentName(channelName, room: room)
        name = currentName
    }

    /// A group chat without a name shows its address's local part, which
    /// is not a name to edit.
    static func currentName(_ channelName: String?, room: BareJID) -> String {
        let trimmed = channelName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed == room.localpart ? "" : trimmed
    }

    var canSave: Bool {
        !isSaving && name.trimmingCharacters(in: .whitespacesAndNewlines) != currentName
    }

    /// True once the server has the new name.
    func save(using session: SessionCoordinator) async -> Bool {
        guard canSave else { return false }
        isSaving = true
        saveError = nil
        defer { isSaving = false }
        do {
            try await session.renameGroupDM(room, to: name)
            return true
        } catch {
            saveError = ActionErrorCopy.message(for: error, fallback: "Couldn't rename the group chat. Try again.")
            return false
        }
    }
}
