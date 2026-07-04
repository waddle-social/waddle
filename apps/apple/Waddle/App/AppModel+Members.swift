import Foundation

// MARK: - Member Management

extension AppModel {
    func addMember(userID: String, role: String = "member") async {
        guard let session else { return }
        do {
            try await client.addMember(sessionID: session.sessionID, userID: userID, role: role)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func removeMember(userID: String) async {
        guard let session else { return }
        do {
            try await client.removeMember(sessionID: session.sessionID, userID: userID)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func changeMemberRole(userID: String, role: String) async {
        guard let session else { return }
        do {
            try await client.updateMemberRole(sessionID: session.sessionID, userID: userID, role: role)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
