import Foundation

// MARK: - Room Loading & MUC Joins

extension AppModel {
    /// Loads the XEP-0503 spaces topology and member list.
    func loadRooms() async {
        if isStructureLoadRunning {
            structureLoadRerunRequested = true
            await withCheckedContinuation { continuation in
                structureLoadWaiters.append(continuation)
            }
            return
        }

        isStructureLoadRunning = true
        defer {
            isStructureLoadRunning = false
            let waiters = structureLoadWaiters
            structureLoadWaiters.removeAll()
            waiters.forEach { $0.resume() }
        }

        repeat {
            structureLoadRerunRequested = false
            await performLoadRooms()
        } while structureLoadRerunRequested
    }

    private func performLoadRooms() async {
        guard let session, let rustClient else {
            updateChatSurfaceState()
            return
        }

        let loadingSessionID = session.sessionID
        structureLoadGeneration += 1
        let loadingGeneration = structureLoadGeneration
        isLoadingStructure = true
        updateChatSurfaceState()
        defer {
            if structureLoadGeneration == loadingGeneration {
                isLoadingStructure = false
                updateChatSurfaceState()
            }
        }

        let topologyResult = await rustClient.discoverTopology()
        let loadedMembersValue: [MemberSummary]
        let memberRefreshError: String?
        do {
            loadedMembersValue = try await client.listMembers(sessionID: session.sessionID)
            memberRefreshError = nil
        } catch {
            loadedMembersValue = members
            memberRefreshError = error.localizedDescription
        }
        dlog(" loadRooms: \(topologyResult.topology.spaces.count) spaces, \(topologyResult.topology.channels.count) rooms, \(loadedMembersValue.count) members")

        guard self.session?.sessionID == loadingSessionID, self.rustClient === rustClient else {
            return
        }

        let discoveredSpaces = topologyResult.topology.spaces.map { space in
            SpaceSummary(
                id: space.id,
                name: space.name,
                description: space.description
            )
        }

        let discoveredChannels = topologyResult.topology.channels
            .map { channel in
                return ChannelSummary(
                    id: channel.id,
                    apiID: parseManagedRoomBareJID(channel.roomJID),
                    roomJid: channel.roomJID,
                    name: channel.name,
                    description: channel.description,
                    channelType: channel.channelType,
                    position: channel.position,
                    spaceID: channel.spaceID
                )
            }
            .sorted {
                ($0.position ?? 0, $0.name.lowercased()) < ($1.position ?? 0, $1.name.lowercased())
            }
        var loadWarning: String?
        if let discoveryError = topologyResult.errorDescription {
            if !spaces.isEmpty || !channels.isEmpty {
                let message = "Space discovery failed; keeping the current topology."
                errorMessage = discoveryError
                loadWarning = message
                structureLoadSurfaceError = nil
            } else {
                errorMessage = discoveryError
                loadWarning = "Space discovery failed."
                structureLoadSurfaceError = (
                    title: "Space discovery failed",
                    message: "Reconnect to retry loading spaces and channels."
                )
                spaces = []
                channels = []
            }
        } else {
            structureLoadSurfaceError = nil
            spaces = discoveredSpaces
            channels = discoveredChannels
        }
        if let memberRefreshError {
            errorMessage = memberRefreshError
            loadWarning = "Members could not be refreshed: \(memberRefreshError)"
        }
        members = loadedMembersValue.sorted { $0.username.lowercased() < $1.username.lowercased() }

        if let selectedChannelID,
           let selectedChannel = channels.first(where: { $0.id == selectedChannelID }) {
            self.selectedChannelID = selectedChannelID
            selectedSpaceID = selectedChannel.spaceID
        } else {
            self.selectedChannelID = channels.first?.id
            selectedSpaceID = channels.first?.spaceID ?? spaces.first?.id
        }
        spaceName = selectedSpace?.name ?? serverURL.host ?? "Waddle"

        syncChatRooms()
        syncChatMembers()
        syncChatMessages()
        updateChatSurfaceState()

        if let channelID = self.selectedChannelID {
            await selectChannel(channelID)
        }

        if let loadWarning {
            chatStore.setBannerState(.error(message: loadWarning))
        }
    }

    func joinSelectedChannel() async throws {
        guard let session,
              let selectedChannelID,
              let rustClient else {
            throw ChatSendError.noRoom
        }

        guard let roomJID = channels.first(where: { $0.id == selectedChannelID })?.roomJid,
              !roomJID.isEmpty else {
            throw ChatSendError.noRoom
        }

        dlog("joinSelectedChannel: roomJID=\(roomJID) nick=\(session.username) alreadyJoined=\(joinedRoomJIDs.contains(roomJID))")
        if joinedRoomJIDs.contains(roomJID) {
            if roomJID == currentRoomJID {
                let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
                chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
            }
            return
        }

        updateConnectionBanner(for: .ready)
        await rustClient.joinRoom(roomJID, nick: session.username)
        try await waitForRoomJoin(roomJID: roomJID, nick: session.username)

        if roomJID == currentRoomJID {
            let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
            chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
        }
    }

    private func waitForRoomJoin(roomJID: String, nick: String) async throws {
        if joinedRoomJIDs.contains(roomJID) {
            return
        }

        let key = roomJoinKey(roomJID: roomJID, nick: nick)
        if roomJoinContinuations[key] != nil {
            return
        }

        try await withCheckedThrowingContinuation { continuation in
            roomJoinContinuations[key] = continuation
            roomJoinTimeoutTasks.removeValue(forKey: key)?.cancel()
            roomJoinTimeoutTasks[key] = Task { [weak self] in
                do {
                    try await Task.sleep(nanoseconds: 30_000_000_000)
                } catch {
                    // Task was cancelled (join completed or connection dropped); do not time out.
                    return
                }
                await self?.handleRoomJoinTimeout(key: key, roomJID: roomJID)
            }
        }
    }

    func roomJoinKey(roomJID: String, nick: String) -> String {
        "\(roomJID)|\(nick.lowercased())"
    }

    @discardableResult
    func finishPendingRoomJoin(key: String) -> Bool {
        roomJoinTimeoutTasks.removeValue(forKey: key)?.cancel()
        guard let continuation = roomJoinContinuations.removeValue(forKey: key) else {
            return false
        }
        continuation.resume()
        return true
    }

    func failPendingRoomJoin(key: String, error: Error) {
        roomJoinTimeoutTasks.removeValue(forKey: key)?.cancel()
        guard let continuation = roomJoinContinuations.removeValue(forKey: key) else {
            return
        }
        continuation.resume(throwing: error)
    }

    func failPendingRoomJoins(with error: Error) {
        for task in roomJoinTimeoutTasks.values {
            task.cancel()
        }
        roomJoinTimeoutTasks.removeAll()

        let continuations = roomJoinContinuations.values
        roomJoinContinuations.removeAll()
        for continuation in continuations {
            continuation.resume(throwing: error)
        }
    }

    private func handleRoomJoinTimeout(key: String, roomJID: String) {
        failPendingRoomJoin(
            key: key,
            error: XMPPServiceError.timeout("Timed out joining \(roomJID).")
        )
    }
}
