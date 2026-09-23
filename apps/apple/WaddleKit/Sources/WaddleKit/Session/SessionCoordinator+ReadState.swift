import Foundation

/// The ids one displayed dispatch needs.
struct DisplayedTarget: Equatable {
    /// XEP-0333 `<displayed id=…/>`: the room-assigned stanza id in a room,
    /// the author-assigned id in 1:1.
    let markerID: String
    /// XEP-0490 pair: room-assigned in a room, assigned by our own account
    /// in 1:1. Nil when the row carries no trusted pair.
    let cursor: DisplayedCursor?
    /// XEP-0333: the sender asked for markers (1:1 only; the archive does
    /// not carry the request).
    let markerRequested: Bool
}

extension SessionCoordinator {
    /// XEP-0430 hydrate, oldest first so the most recent DM ends on top.
    func hydrateInbox() async {
        guard let entries = try? await port.fetchInbox() else { return }
        for entry in entries.sorted(by: { ($0.lastUpdated ?? .min) < ($1.lastUpdated ?? .min) }) {
            applyInbox(entry)
        }
    }

    /// XEP-0490 bootstrap: seed cursors, then subscribe for sibling updates.
    func bootstrapDisplayedCursors() async {
        if let cursors = try? await port.fetchDisplayedCursors() {
            cursors.forEach(applyDisplayedCursor)
        }
        _ = await port.subscribeDisplayedCursors()
    }

    /// A cursor from another device: advance ours and recompute the badge
    /// from the loaded timeline. A cursor whose target is not loaded is
    /// ignored; the next open recomputes anyway.
    func applyDisplayedCursor(_ cursor: DisplayedCursor) {
        let conversation: ConversationID = directory.isRoom(cursor.conversation)
            ? .room(cursor.conversation)
            : .direct(cursor.conversation)
        // XEP-0490: a room cursor names the room's own stanza id, a 1:1
        // cursor our archive's. Any other authority is not trusted.
        let authority = conversation.isRoom ? conversation.jid : account.jid
        guard cursor.stanzaIDBy == authority else { return }
        let items = timelines.timeline(for: conversation).items
        // Match only on the id the cursor's authority assigned: the room's
        // stanza id in a room, our own archive's id in 1:1.
        guard let index = items.lastIndex(where: { $0.identity.stanzaID(assignedBy: cursor.stanzaIDBy) == cursor.stanzaID })
        else { return }
        let current = readCursors.cursor(conversation)
        let currentIndex = current.flatMap { id in items.lastIndex(where: { $0.id == id || $0.identity.all.contains(id) }) } ?? -1
        guard currentIndex < index else { return }
        guard readCursors.advance(conversation, from: current, to: cursor.stanzaID) else { return }
        // Loaded rows only give the true count when they reach the present;
        // after a gap the server inbox count stays authoritative.
        guard history.state(of: conversation).hasLoadedLatest else { return }
        let remaining = items[(index + 1)...].filter { !$0.isMine && $0.tombstone == nil && $0.isFeedVisible }.count
        unread.set(remaining, for: conversation)
    }

    /// Marks the newest visible message of `conversation` as read: clears
    /// the badge, sends the XEP-0333 marker (when read receipts are on and
    /// the XEP allows it), publishes the XEP-0490 cursor, and tells the
    /// inbox. Offline reads are parked and replayed on the next session.
    public func markDisplayed(_ conversation: ConversationID) async {
        unread.clear(conversation)
        inbox.markRead(conversation.jid)
        guard let target = newestDisplayedTarget(in: conversation) else { return }
        let before = readCursors.cursor(conversation)
        guard before != target.markerID else { return }
        guard connection == .online else {
            pendingDisplayed.insert(conversation)
            return
        }
        var succeeded = true
        if sendsReadReceipts, markerAllowed(target, in: conversation) {
            succeeded = await port.sendDisplayed(stanzaID: target.markerID, in: conversation) && succeeded
        }
        if let cursor = target.cursor, await supportsCursorPublish() {
            succeeded = await port.publishDisplayedCursor(cursor) && succeeded
        }
        try? await port.markInboxRead(partner: conversation.jid, threadID: nil)
        if succeeded {
            readCursors.advance(conversation, from: before, to: target.markerID)
        }
    }

    /// Marks read only if the user is still looking at `conversation`:
    /// callers that awaited in between (history loads, app activation) must
    /// not mark a conversation the user already left.
    func markDisplayedIfVisible(_ conversation: ConversationID) async {
        guard isAppActive,
              visibleConversation == conversation,
              unread.activeConversation == conversation
        else { return }
        await markDisplayed(conversation)
    }

    func drainPendingDisplayed() async {
        let pending = pendingDisplayed
        pendingDisplayed.removeAll()
        for conversation in pending {
            await markDisplayed(conversation)
        }
    }

    /// Only feed-visible rows count: a thread reply never opened must not
    /// advance the cursor.
    func newestDisplayedTarget(in conversation: ConversationID) -> DisplayedTarget? {
        let items = timelines.timeline(for: conversation).items
        guard let item = items.last(where: { !$0.isMine && $0.tombstone == nil && $0.isFeedVisible && !$0.isLocalEcho })
        else { return nil }
        let identity = item.identity
        if conversation.isRoom {
            guard let id = identity.stanzaID(assignedBy: conversation.jid) else { return nil }
            return DisplayedTarget(
                markerID: id,
                cursor: DisplayedCursor(conversation: conversation.jid, stanzaID: id, stanzaIDBy: conversation.jid),
                markerRequested: false
            )
        }
        // XEP-0333: the marker copies the message's `@id`.
        guard let markerID = identity.messageID ?? identity.originID else { return nil }
        let authorities = [account.jid] + [BareJID(localpart: nil, domain: account.jid.domain)].compactMap { $0 }
        let cursor = authorities.lazy.compactMap { authority in
            identity.stanzaID(assignedBy: authority).map {
                DisplayedCursor(conversation: conversation.jid, stanzaID: $0, stanzaIDBy: authority)
            }
        }.first
        return DisplayedTarget(
            markerID: markerID,
            cursor: cursor,
            markerRequested: item.message.isLive && item.message.displayedMarkerRequested
        )
    }

    /// XEP-0333: rooms always accept markers on room-assigned ids; 1:1
    /// markers go only to senders who requested them.
    private func markerAllowed(_ target: DisplayedTarget, in conversation: ConversationID) -> Bool {
        conversation.isRoom || target.markerRequested
    }

    /// XEP-0490 §3 requires publish-options support; probed once per
    /// session and retried after a failed probe.
    private func supportsCursorPublish() async -> Bool {
        if let known = mdsPublishSupported { return known }
        let supported = await port.supportsDisplayedCursorPublish()
        mdsPublishSupported = supported
        return supported
    }
}
