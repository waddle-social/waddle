import Foundation

// MARK: - Spaces & Channels

extension AppModel {
    func reloadRooms() async {
        await loadRooms()
    }

    func selectChannel(_ channelID: String?) async {
        dlog(" selectChannel: \(channelID ?? "nil")")
        selectedChannelID = channelID
        selectedSpaceID = channels.first(where: { $0.id == channelID })?.spaceID ?? selectedSpaceID
        selectedForumThreadID = nil
        syncChatRooms()
        syncChatMembers()
        syncChatMessages()
        updateChatSurfaceState()

        guard channelID != nil else {
            return
        }

        do {
            try await joinSelectedChannel()
            dlog(" joined, loading history for roomJID=\(self.currentRoomJID ?? "nil")")
            await chatStore.refreshSelectedRoomHistory()
            syncChatMessages()
            dlog(" history done: store=\(self.chatStore.messages.count) cached=\(self.messagesByRoomJID[self.currentRoomJID ?? ""]?.count ?? 0)")
            updateChatSurfaceState()
        } catch {
            dlog(" selectChannel error: \(error)")
            errorMessage = error.localizedDescription
            chatStore.setBannerState(.error(message: error.localizedDescription))
            chatStore.failRoomHistoryLoad(error.localizedDescription)
            updateChatSurfaceState()
        }
    }

    func reloadSelectedSpaceStructure() async {
        await loadRooms()
    }

    func createSpace(name: String, description: String?) async {
        guard let session else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorMessage = "Space name is required."
            return
        }

        isCreatingSpace = true
        defer { isCreatingSpace = false }

        do {
            try await client.createSpace(
                sessionID: session.sessionID,
                name: trimmedName,
                description: description
            )
            await loadRooms()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func updateSpace(name: String, description: String?) async {
        guard let session else { return }
        do {
            try await client.updateSpace(sessionID: session.sessionID, name: name, description: description)
            spaceName = name
            if let selectedSpaceID,
               let index = spaces.firstIndex(where: { $0.id == selectedSpaceID }) {
                spaces[index] = SpaceSummary(id: selectedSpaceID, name: name, description: description)
            } else if spaces.count == 1 {
                spaces[0] = SpaceSummary(id: spaces[0].id, name: name, description: description)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func deleteSpace() async {
        guard let session else { return }
        do {
            try await client.deleteSpace(sessionID: session.sessionID)
            spaceName = nil
            spaces = []
            selectedSpaceID = nil
            channels = []
            selectedChannelID = nil
            members = []
            updateChatSurfaceState()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func updateChannel(channelID: String, name: String, description: String?, position: Int) async {
        guard let session else { return }
        let apiChannelID = channels.first(where: { $0.id == channelID })?.apiID ?? channelID
        do {
            try await client.updateChannel(sessionID: session.sessionID, channelID: apiChannelID, name: name, description: description, position: position)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func createChannel(name: String, description: String?, channelType: String) async {
        guard let rustClient else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorMessage = "Channel name is required."
            return
        }

        isCreatingChannel = true
        defer { isCreatingChannel = false }

        let position = channels.count
        let result = await rustClient.createChannel(
            name: trimmedName,
            description: description,
            channelType: channelType,
            position: position
        )
        await loadRooms()
        if let channelID = result?.channelID {
            await selectChannel(channelID)
        }
    }
}
