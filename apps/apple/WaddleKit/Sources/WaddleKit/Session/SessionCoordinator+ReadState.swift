import Foundation

/// A read parked offline, with the newest stanza id it covered.
struct PendingInboxRead {
    let covered: String?
}

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

/// One server inbox read: a conversation's row, or one room thread's row.
struct InboxReadKey: Hashable {
    let partner: BareJID
    let threadID: String?
}

extension SessionCoordinator {
    /// XEP-0430 hydrate, oldest first so the most recent DM ends on top.
    /// Returns false when the fetch failed. Reads parked while offline are
    /// replayed only after a hydrate, against the fresh server state.
    @discardableResult
    func hydrateInbox() async -> Bool {
        let epoch = connectionEpoch
        guard let entries = try? await port.fetchInbox() else { return false }
        guard epoch == connectionEpoch else { return false }
        for entry in entries.sorted(by: { ($0.lastUpdated ?? .min) < ($1.lastUpdated ?? .min) }) {
            applyInbox(entry)
        }
        await drainPendingInboxReads()
        return true
    }

    /// Re-fetches the server inbox (pull to refresh).
    public func refreshInbox() async {
        guard connection == .online else { return }
        await hydrateInbox()
    }

    /// Retries a failed hydrate off the ready pipeline, so sends are not
    /// held behind it: after `inboxHydrateRetryDelays` (2 s, then 8 s),
    /// abandoned if the stream changes.
    func scheduleInboxHydrate(delays: [TimeInterval]? = nil) {
        guard inboxHydrateTask == nil else { return }
        let delays = delays ?? inboxHydrateRetryDelays
        let epoch = connectionEpoch
        inboxHydrateTask = Task { [weak self] in
            for delay in delays {
                try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
                // Cancelled when the stream changes, which also frees the slot.
                guard let self, !Task.isCancelled else { return }
                guard epoch == self.connectionEpoch, self.connection == .online else { break }
                if await self.hydrateInbox() { break }
            }
            guard let self, !Task.isCancelled else { return }
            self.inboxHydrateTask = nil
        }
    }

    /// A new or lost stream: a retry scheduled for the old one must not
    /// hold the slot the next session's retry needs.
    func cancelInboxHydrate() {
        inboxHydrateTask?.cancel()
        inboxHydrateTask = nil
    }

    /// XEP-0490 bootstrap: seed cursors, then subscribe for sibling updates.
    func bootstrapDisplayedCursors() async {
        if let cursors = try? await port.fetchDisplayedCursors() {
            cursors.forEach(applyDisplayedCursor)
        }
        _ = await port.subscribeDisplayedCursors()
    }

    /// Who assigns the XEP-0359 ids of our 1:1 archive: the account, or a
    /// server that stamps its domain. Publish and apply share this list.
    var directArchiveAuthorities: [BareJID] {
        [account.jid] + [BareJID(localpart: nil, domain: account.jid.domain)].compactMap { $0 }
    }

    /// A cursor from another device: advance ours and recompute the badge
    /// from the loaded timeline. A cursor whose target is not loaded is
    /// ignored; the next open recomputes anyway.
    func applyDisplayedCursor(_ cursor: DisplayedCursor) {
        let conversation: ConversationID = directory.isRoom(cursor.conversation)
            ? .room(cursor.conversation)
            : .direct(cursor.conversation)
        // XEP-0490: a room cursor names the room's own stanza id, a 1:1
        // cursor our archive's (the same authorities we publish with). Any
        // other authority is not trusted.
        let trusted = conversation.isRoom ? [conversation.jid] : directArchiveAuthorities
        guard trusted.contains(cursor.stanzaIDBy) else { return }
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

    /// Marks `conversation` read: clears the badge, tells the server inbox
    /// (whenever it or the local badge still counts anything, since thread
    /// replies and reactions bump the server row without a new feed row to
    /// mark), and sends the XEP-0333 marker and XEP-0490 cursor for the
    /// newest visible message. Offline, both are parked for the next
    /// session.
    public func markDisplayed(_ conversation: ConversationID) async {
        // `open` clears the badge and the local row before calling here, so
        // the decision rests on what the server last reported.
        let needsServerRead = inbox.isUnreadOnServer(conversation.jid) || unread.count(for: conversation) > 0
        unread.clear(conversation)
        let covered = inbox.markRead(conversation.jid)
        if needsServerRead {
            await sendInboxRead(InboxReadKey(partner: conversation.jid, threadID: nil), covering: covered)
        }
        await dispatchDisplayed(conversation)
    }

    /// Marks one room thread read on the server inbox's thread row.
    public func markThreadRead(_ thread: ThreadKey) async {
        let needsServerRead = inbox.isUnreadOnServer(thread.room, threadID: thread.threadID)
            || unread.threadCount(for: thread) > 0
        unread.clearThread(thread)
        let covered = inbox.markRead(thread.room, threadID: thread.threadID)
        if needsServerRead {
            await sendInboxRead(InboxReadKey(partner: thread.room, threadID: thread.threadID), covering: covered)
        }
    }

    /// The Waddle `<mark-read/>` IQ. It clears the whole row, so a read
    /// parked offline records the newest stanza id it covered and is sent
    /// later only if nothing newer arrived. A rejected read drops the
    /// barrier and re-fetches the inbox, so the badge shows the server's
    /// count instead of a local zero the server never took.
    func sendInboxRead(_ key: InboxReadKey, covering covered: String?) async {
        guard connection == .online else {
            pendingInboxReads[key] = PendingInboxRead(covered: covered)
            return
        }
        let epoch = connectionEpoch
        do {
            try await port.markInboxRead(partner: key.partner, threadID: key.threadID)
            guard epoch == connectionEpoch else { return }
            inbox.serverTookRead(key.partner, threadID: key.threadID)
            failedInboxReads.remove(key)
        } catch {
            guard epoch == connectionEpoch else { return }
            inbox.forgetBarrier(key.partner, threadID: key.threadID)
            // The re-fetch brings the count back; the row on screen must not
            // read it again automatically, or a server that keeps refusing
            // the read would get it in a tight loop. The next open or new
            // message retries.
            failedInboxReads.insert(key)
            scheduleInboxHydrate()
        }
    }

    func drainPendingInboxReads() async {
        let pending = pendingInboxReads
        pendingInboxReads.removeAll()
        for (key, read) in pending {
            guard let current = inbox.entry(for: key.partner, threadID: key.threadID),
                  current.lastStanzaID == read.covered
            else { continue }
            await sendInboxRead(key, covering: read.covered)
        }
    }

    /// XEP-0333 marker and XEP-0490 cursor for the newest visible message,
    /// skipped when the cursor already names it.
    func dispatchDisplayed(_ conversation: ConversationID) async {
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

    /// Markers parked while offline. Their inbox reads replay separately,
    /// bounded, after the next hydrate.
    func drainPendingDisplayed() async {
        let pending = pendingDisplayed
        pendingDisplayed.removeAll()
        for conversation in pending {
            await dispatchDisplayed(conversation)
        }
    }

    /// Marks the thread read only if it is still on screen in an active app.
    func markThreadReadIfVisible(_ thread: ThreadKey) async {
        guard isAppActive, visibleThread == thread, unread.activeThread == thread else { return }
        await markThreadRead(thread)
    }

    /// Only feed-visible rows count: a thread reply never opened must not
    /// advance the cursor. An archived row counts only once the newest page
    /// is loaded, so a cursor never moves back to a row that merely happens
    /// to be the newest one loaded.
    func newestDisplayedTarget(in conversation: ConversationID) -> DisplayedTarget? {
        let items = timelines.timeline(for: conversation).items
        guard let item = items.last(where: { !$0.isMine && $0.tombstone == nil && $0.isFeedVisible && !$0.isLocalEcho })
        else { return nil }
        guard item.message.isLive || history.state(of: conversation).hasLoadedLatest else { return nil }
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
        let cursor = directArchiveAuthorities.lazy.compactMap { authority in
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
