import Foundation

/// Pinned messages (`urn:waddle:pin:0`): which conversations have them,
/// who may pin, by which id, and keeping the list.
extension SessionCoordinator {
    /// Whether the account may pin messages in `conversation`. In a room,
    /// as web's `currentUserCanPin` decides: only in a room we are in;
    /// owners and admins may; any occupant may where the room lets anyone
    /// pin; unknown while the room's disco#info has not said. In a
    /// conversation of two either person may, as on web.
    public func pinAccess(in conversation: ConversationID) -> PinAccess {
        guard conversation.isRoom else { return keepsPins(conversation) ? .allowed : .denied }
        guard presence.joinedRooms.contains(conversation.jid),
              let occupant = selfOccupant(in: conversation.jid) else { return .denied }
        if occupant.affiliation == .owner || occupant.affiliation == .admin { return .allowed }
        switch directory.channel(for: conversation.jid)?.pinPermission {
        case .anyone: return .allowed
        case .adminsOnly: return .denied
        case nil: return .unknown
        }
    }

    /// Whether the account may pin `item`: as its conversation allows, and
    /// not while the message is still on its way. The server cannot pin
    /// what it has not seen, and would not say so. In a conversation of
    /// two our own message stays the row we made for it, so what says the
    /// server has it is its acknowledgement.
    public func pinAccess(for item: TimelineItem) -> PinAccess {
        if item.isLocalEcho, deliveries.state(of: item.id) != .acknowledged { return .denied }
        return pinAccess(in: item.conversation)
    }

    /// Whether `conversation` has pins this server keeps: a room, or a
    /// conversation of two with someone on our own server. The server
    /// keeps none with a person elsewhere, and a query for them would go
    /// to their server and tell it when the conversation was opened.
    func keepsPins(_ conversation: ConversationID) -> Bool {
        conversation.isRoom || conversation.jid.domain == account.jid.domain
    }

    /// Reads who may pin in `room` again after the room said its
    /// configuration changed (XEP-0045 §10.2.1). If the room cannot be
    /// read, what was known may be stale, so nothing is assumed.
    func readPinPermissionAgain(of room: BareJID) {
        guard !isStopped, directory.channel(for: room) != nil else { return }
        let epoch = connectionEpoch
        Task { @MainActor [self] in
            let read: PinPermission?
            do {
                read = try await port.fetchRoomPinPermission(of: room)
            } catch {
                read = nil
            }
            // Asked on a connection that has since dropped: the new
            // connection's lookup read the room afresh.
            guard !isStopped, epoch == connectionEpoch, let channel = directory.channel(for: room) else { return }
            directory.upsert(channel.withPinPermission(read))
            directoryMutations.append(.setPinPermission(room, read))
        }
    }

    /// Whether a notice that something was pinned in `conversation` is
    /// taken. A private message from a room's occupant is addressed like a
    /// conversation of two with the room, which is not one that has pins.
    func takesPinNotice(in conversation: ConversationID) -> Bool {
        keepsPins(conversation) && (conversation.isRoom || directory.channel(for: conversation.jid) == nil)
    }

    public func setPinned(_ pinned: Bool, _ item: TimelineItem) async -> Bool {
        guard let target = item.pinTargetID else { return false }
        return await port.setPinned(pinned, targetID: target, in: item.conversation)
    }

    /// Unpins by the pin's own id, whether or not its message is loaded.
    public func unpin(_ pin: PinEntry, in conversation: ConversationID) async -> Bool {
        guard pinAccess(in: conversation) == .allowed else { return false }
        return await port.setPinned(false, targetID: pin.targetStanzaID, in: conversation)
    }

    public func isPinned(_ item: TimelineItem) -> Bool {
        item.pinAliases.contains { pins.isPinned($0, in: item.conversation.jid) }
    }

    /// Reads the pins of a room, or of a conversation of two from the other person's address.
    func refreshPins(in conversation: BareJID) async {
        let version = pins.versionBeforeFetch(conversation)
        guard let entries = try? await port.fetchPins(in: conversation) else { return }
        pins.seed(entries, in: conversation, fetchedAt: version)
    }
}
