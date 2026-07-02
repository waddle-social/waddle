import Foundation

// MARK: - Presence (MUC & DM)

extension AppModel {
    func handleIncomingPresence(_ event: XMPPPresenceEvent) {
        guard let from = event.from else { return }
        let bareFrom = barePeerJID(from)

        if parseManagedRoomBareJID(bareFrom) == nil {
            let presence = presenceState(from: event)
            dmPresence[bareFrom] = presence
            if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == bareFrom }) {
                chatStore.dmConversations[idx].presenceShow = presence
            }
            return
        }

        let roomJID = bareFrom

        guard let nick = XMPPJID(string: from)?.resource, !nick.isEmpty else {
            return
        }

        var roomPresence = presenceByRoomJID[roomJID] ?? [:]
        if event.type == "unavailable" {
            roomPresence.removeValue(forKey: nick)
        } else {
            roomPresence[nick] = presenceState(from: event)
        }
        presenceByRoomJID[roomJID] = roomPresence

        let eventHats = mergedPresenceHats(from: event)
        if !eventHats.isEmpty {
            var roomHats = hatsByRoomJID[roomJID] ?? [:]
            roomHats[nick] = eventHats
            hatsByRoomJID[roomJID] = roomHats
            refreshHatTitles(in: roomJID, for: nick)
        }

        dlog("presence: room=\(roomJID) nick=\(nick) type=\(event.type ?? "nil") sessionUser=\(session?.username ?? "nil") match=\(session?.username == nick)")
        if session?.username == nick {
            let joinKey = roomJoinKey(roomJID: roomJID, nick: nick)
            dlog("presence: self-presence! joinKey=\(joinKey) pendingKeys=\(Array(roomJoinContinuations.keys))")
            if event.type == "unavailable" {
                joinedRoomJIDs.remove(roomJID)
                failPendingRoomJoin(key: joinKey, error: XMPPServiceError.disconnected)
            } else {
                joinedRoomJIDs.insert(roomJID)
                let wasPending = finishPendingRoomJoin(key: joinKey)
                if roomJID == currentRoomJID {
                    let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
                    chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
                    if !wasPending {
                        // Self-presence arrived after the join timeout fired; load history now.
                        Task {
                            await chatStore.refreshSelectedRoomHistory()
                            syncChatMessages()
                            updateChatSurfaceState()
                        }
                    }
                }
            }
        }

        if roomJID == currentRoomJID {
            syncChatMembers()
            syncChatMessages()
        }
    }

    private func mergedPresenceHats(from event: XMPPPresenceEvent) -> [XMPPPresenceHat] {
        var hats: [XMPPPresenceHat] = []
        if event.mucAffiliation == .owner {
            hats.append(XMPPPresenceHat(uri: "urn:xmpp:hats:owner", title: "Owner"))
        } else if event.mucAffiliation == .admin {
            hats.append(XMPPPresenceHat(uri: "urn:xmpp:hats:admin", title: "Admin"))
        }
        if event.mucRole == .moderator {
            hats.append(XMPPPresenceHat(uri: "urn:xmpp:hats:moderator", title: "Moderator"))
        }
        for hat in event.hats where !hats.contains(where: { $0.uri == hat.uri }) {
            hats.append(hat)
        }
        return hats
    }

    private func refreshHatTitles(in roomJID: String, for nick: String) {
        guard let titles = hatsByRoomJID[roomJID]?[nick]?.map(\.title) else { return }
        guard var messages = messagesByRoomJID[roomJID] else { return }
        var changed = false
        for index in messages.indices where messages[index].senderDisplayName == nick {
            messages[index].hatTitles = titles
            changed = true
        }
        if changed {
            messagesByRoomJID[roomJID] = messages
        }
    }

    private func presenceState(from event: XMPPPresenceEvent) -> ChatPresenceState {
        if event.type == "unavailable" {
            return .offline
        }

        switch event.show?.lowercased() {
        case "away", "xa":
            return .away
        case "dnd":
            return .dnd
        case nil, "", "chat":
            return .available
        case let value?:
            return .unknown(value)
        }
    }
}
