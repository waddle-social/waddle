import Foundation
import Observation
import WaddleKit

/// The rename sheet's draft. An empty name removes the group chat's name.
@MainActor
@Observable
final class GroupRenameEditor {
    let room: BareJID
    /// The name the directory shows for the group chat now.
    let currentName: String
    var name: String
    private(set) var isSaving = false
    private(set) var saveError: String?

    init(room: BareJID, channelName: String?) {
        self.room = room
        currentName = channelName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        name = currentName
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
