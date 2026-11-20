import Foundation
import Observation
import WaddleKit

/// The edit room sheet: loads the room's settings, holds the draft and
/// saves only what changed.
@MainActor
@Observable
final class RoomSettingsEditor {
    enum Phase: Equatable {
        case loading
        case loaded(RoomSettings)
        case failed(String)
    }

    let room: BareJID
    private(set) var phase: Phase
    var name = ""
    var summary = ""
    var pinPermission: PinPermission?
    private(set) var isSaving = false
    private(set) var saveError: String?

    /// `phase` lets a preview or snapshot start loaded or failed.
    init(room: BareJID, phase: Phase = .loading) {
        self.room = room
        self.phase = .loading
        if case let .loaded(settings) = phase {
            fill(from: settings)
        }
        self.phase = phase
    }

    var settings: RoomSettings? {
        if case let .loaded(settings) = phase { return settings }
        return nil
    }

    var patch: RoomSettingsPatch {
        guard let settings else { return RoomSettingsPatch() }
        return RoomSettingsPatch.changes(from: settings, name: name, summary: summary, pinPermission: pinPermission)
    }

    var isNameMissing: Bool {
        name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var canSave: Bool {
        settings != nil && !isSaving && !isNameMissing && !patch.isEmpty
    }

    func load(using session: SessionCoordinator) async {
        phase = .loading
        do {
            let settings = try await session.roomSettings(of: room)
            fill(from: settings)
            phase = .loaded(settings)
        } catch {
            phase = .failed(ActionErrorCopy.message(for: error, fallback: "Check your connection and try again."))
        }
    }

    /// True once the server has the changes.
    func save(using session: SessionCoordinator) async -> Bool {
        guard canSave else { return false }
        isSaving = true
        saveError = nil
        defer { isSaving = false }
        do {
            try await session.updateRoom(room, with: patch)
            return true
        } catch {
            saveError = ActionErrorCopy.message(for: error, fallback: "Couldn't save the changes. Try again.")
            return false
        }
    }

    private func fill(from settings: RoomSettings) {
        name = settings.name ?? ""
        summary = settings.summary ?? ""
        pinPermission = settings.pinPermission
    }
}
