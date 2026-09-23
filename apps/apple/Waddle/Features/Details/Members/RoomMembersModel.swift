import Foundation
import Observation
import WaddleKit

/// Affiliation list loading and moderation for one room's details screen.
@MainActor
@Observable
final class RoomMembersModel {
    let room: BareJID
    /// XEP-0045 affiliation lists; nil until "Load all members".
    private(set) var members: [RoomMember]?
    private(set) var isLoading = false
    private(set) var loadError: String?
    /// The last moderation failure, shown as an alert.
    var actionError: String?
    /// A ban awaiting confirmation.
    var pendingBan: MemberSubject?

    init(room: BareJID) {
        self.room = room
    }

    func loadMembers(using session: SessionCoordinator) async {
        guard !isLoading else { return }
        isLoading = true
        loadError = nil
        do {
            members = try await session.members(of: room)
        } catch {
            loadError = ActionErrorCopy.message(for: error, fallback: "Couldn't load members. Try again.")
        }
        isLoading = false
    }

    /// Affiliated members not in the room, once loaded.
    func absentMembers(present occupants: [Occupant]) -> [RoomMember]? {
        members.map { MemberRoster.absent($0, present: occupants) }
    }

    /// Bans wait for confirmation; everything else runs now.
    func request(_ action: MemberAction, on subject: MemberSubject, using session: SessionCoordinator) {
        if action == .ban {
            pendingBan = subject
            return
        }
        perform(action, on: subject, using: session)
    }

    func confirmBan(using session: SessionCoordinator) {
        guard let subject = pendingBan else { return }
        pendingBan = nil
        perform(.ban, on: subject, using: session)
    }

    private func perform(_ action: MemberAction, on subject: MemberSubject, using session: SessionCoordinator) {
        actionError = nil
        Task {
            do {
                try await apply(action, on: subject, using: session)
            } catch {
                actionError = ActionErrorCopy.message(for: error, fallback: "That didn't work. Try again.")
            }
        }
    }

    private func apply(_ action: MemberAction, on subject: MemberSubject, using session: SessionCoordinator) async throws {
        if let affiliation = action.affiliation {
            guard let jid = subject.jid else { return }
            try await session.setAffiliation(affiliation, of: jid, in: room)
            if let loaded = members {
                members = MemberRoster.updating(loaded, jid: jid, to: affiliation)
            }
        } else if let nick = subject.nick {
            try await session.kick(nick: nick, from: room)
        }
    }
}
