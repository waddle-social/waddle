import Foundation

extension SessionCoordinator {
    static let maxConcurrentAutoJoins = 4

    /// Loads the account's contact list. A lookup that fails keeps the
    /// list already held (an older list is nearer the truth than none)
    /// and is recorded, so a screen can say so and offer to try again.
    public func refreshContacts() async {
        contactsLoad = .loading
        let loaded = try? await port.fetchContacts()
        guard !isStopped, !Task.isCancelled else { return }
        if let loaded {
            contacts = loaded
            contactsLoad = .idle
        } else {
            contactsLoad = .failed
        }
    }

    /// Rediscovers spaces and rooms, then joins every autojoin room, group
    /// DM, and room the user opened or created this session. A failed
    /// discovery keeps the last known directory.
    ///
    /// A fresh connection is in no room, so every one is joined. A look
    /// taken mid-session (`joiningOnlyNewRooms`) leaves the rooms we are
    /// already in alone: joining again makes the server send everyone in
    /// each of them once more.
    ///
    /// Returns what the lookup found, or nil when it failed or a newer
    /// connection made it stale.
    @discardableResult
    func refreshDirectory(joiningOnlyNewRooms: Bool = false) async -> DirectoryLookup? {
        let mutationStart = directoryMutations.count
        directoryRefreshesInFlight += 1
        defer {
            directoryRefreshesInFlight -= 1
            if directoryRefreshesInFlight == 0 { directoryMutations.removeAll() }
        }
        let epoch = connectionEpoch
        lookupsBegun += 1
        let lookupNumber = lookupsBegun
        let result = await port.discoverTopology()
        // A superseded ready pipeline (the stream dropped and reconnected
        // meanwhile) must not overwrite the directory with its answer.
        // Neither must a look asked for elsewhere, whose task a reconnect
        // does not cancel.
        guard !Task.isCancelled, epoch == connectionEpoch else { return nil }
        var lookup: DirectoryLookup?
        let leftBefore = directory.leftRooms.map(\.roomJID)
        if case let .success(topology) = result {
            lookup = DirectoryLookup(
                rooms: Set(topology.channels.map(\.roomJID)), readOwnBookmarks: topology.ownBookmarksLoaded)
            directory.apply(topology)
            // A room that outlived its space is listed again.
            roomsOfDeletedSpaces.subtract(topology.channels.map(\.roomJID))
            unfenceRoomsMadeAgain(listedBy: topology, inLookup: lookupNumber)
            for mutation in directoryMutations.dropFirst(mutationStart) {
                switch mutation {
                case .upsert(let channel):
                    // Queued local edits may carry older discovery flags.
                    let found = directory.channel(for: channel.roomJID)
                    directory.upsert(channel.with(hasLeft: found?.hasLeft ?? channel.hasLeft)
                        .withKnownMembersOnly(found?.membersOnly)
                        .withPinPermission(found?.pinPermission ?? channel.pinPermission))
                case .remove(let room): directory.remove(room)
                case let .setLeft(room, hasLeft):
                    if let found = directory.channel(for: room) { directory.upsert(found.with(hasLeft: hasLeft)) }
                case let .setPinPermission(room, permission):
                    // Saved here, so newer than what the lookup read.
                    if let found = directory.channel(for: room) {
                        directory.upsert(found.withPinPermission(permission))
                    }
                }
            }
        }
        for room in destroyedRooms { directory.remove(room) }
        silenceLeftRooms()
        if lookup?.readOwnBookmarks == false { scheduleDirectoryRetry() }
        // After the local changes above: a rejoin made here while the
        // lookup was on its way must not be answered with a leave.
        var settingsReads: [Task<Void, Never>] = []
        if lookup?.readOwnBookmarks == true {
            await leaveRoomsLeftOnAnotherDevice()
            // The connection may have dropped during that wait. Then the
            // new connection's lookup joins the rooms, not this one.
            guard !Task.isCancelled, epoch == connectionEpoch else { return nil }
            // Started before the joins: a rejoined room's messages should
            // find its settings restored.
            settingsReads = loadNotifySettings(
                ofRejoined: leftBefore.filter { directory.isRoom($0) && !directory.hasLeft($0) })
        }
        // Until the account's bookmarks have been read, any space room may
        // be one it left, and joining that would show it there again. The
        // lookup that reads them joins those rooms. Group chats are never
        // left this way, and rooms opened here were asked for.
        let listed = (directory.channels + directory.groupDMs)
            .filter { $0.isGroupDM || ($0.autojoin && directory.knowsLeftRooms) }
            .map(\.roomJID)
        var seen: Set<BareJID> = []
        let roomsToJoin = (listed + onDemandRooms.sorted(by: { $0.description < $1.description })).filter { room in
            seen.insert(room).inserted && (directory.isRoom(room) || onDemandRooms.contains(room))
                && !directory.hasLeft(room)
                && !(joiningOnlyNewRooms && presence.joinedRooms.contains(room))
        }
        await withTaskGroup(of: Void.self) { group in
            var pending = roomsToJoin[...]
            func addNext() {
                guard let room = pending.popFirst() else { return }
                group.addTask {
                    await self.join(room)
                }
            }
            for _ in 0..<Self.maxConcurrentAutoJoins { addNext() }
            while await group.next() != nil {
                guard !Task.isCancelled else {
                    group.cancelAll()
                    break
                }
                addNext()
            }
        }
        for read in settingsReads { await read.value }
        return lookup
    }

    /// Reads the directory again when a lookup could not read the
    /// account's bookmarks: after each of `directoryRetryDelays`, then
    /// after the last of them again and again, until one reads them.
    /// Until then no space room is joined, so it gives up only when the
    /// stream changes. Off the ready pipeline, so sends are not held
    /// behind it.
    func scheduleDirectoryRetry(delays: [TimeInterval]? = nil) {
        guard directoryRetryTask == nil else { return }
        let delays = delays ?? directoryRetryDelays
        guard let longest = delays.last else { return }
        let epoch = connectionEpoch
        directoryRetryTask = Task { [weak self] in
            for attempt in 0... {
                let delay = attempt < delays.count ? delays[attempt] : longest
                try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
                // Cancelled when the stream changes, which also frees the slot.
                guard let self, !Task.isCancelled else { return }
                guard epoch == self.connectionEpoch, self.connection == .online else { break }
                if await self.refreshDirectory(joiningOnlyNewRooms: true)?.readOwnBookmarks == true { break }
            }
            guard let self, !Task.isCancelled else { return }
            self.directoryRetryTask = nil
        }
    }

    /// A new or lost stream: a retry scheduled for the old one must not
    /// hold the slot the next session's retry needs.
    func cancelDirectoryRetry() {
        directoryRetryTask?.cancel()
        directoryRetryTask = nil
    }

    /// Counts nothing for the rooms the account left.
    func silenceLeftRooms() {
        unread.silence(Set(directory.leftRooms.map(\.roomJID)))
    }

    /// Leaves the rooms this session is in that the account has left on
    /// another device (XEP-0402 §4).
    private func leaveRoomsLeftOnAnotherDevice() async {
        for room in directory.leftRooms.map(\.roomJID) {
            await departIfLeft(room)
        }
    }

    /// Joins `room` if we are not an occupant, and keeps it joined across
    /// reconnects. Rooms whose bookmark says `autojoin=false` (XEP-0402)
    /// are otherwise never entered, so opening one would show a room we
    /// cannot send to or receive from (XEP-0045 §7.4).
    func ensureJoined(_ room: BareJID) async {
        guard !isFenced(room), !directory.hasLeft(room) else { return }
        onDemandRooms.insert(room)
        // Offline, the ready pipeline joins it on connect.
        guard status.connection == .online, !presence.joinedRooms.contains(room) else { return }
        await join(room)
    }

    /// Sends a join unless the room is fenced or left when its turn comes. Joins
    /// to one room run in order, so giving the room up can wait for those
    /// already on their way.
    func join(_ room: BareJID) async {
        let previous = joinTails[room]
        let task = Task { @MainActor [self] in
            await previous?.value
            guard !isFenced(room), !directory.hasLeft(room) else { return }
            await port.joinRoom(room, nick: account.nick)
        }
        joinTails[room] = task
        await task.value
        if joinTails[room] == task { joinTails[room] = nil }
    }

    /// Starts reading the notification settings of rooms the account
    /// joined again on another device. They were off while the rooms were
    /// left, and nothing else reads them again before the next connect.
    /// Until its answer comes a room alerts as by default. Each read
    /// takes its turn among the writes to that room's settings, so a
    /// change made here meanwhile is stored after it and stands. The
    /// rooms are read side by side, not one after another.
    private func loadNotifySettings(ofRejoined rooms: [BareJID]) -> [Task<Void, Never>] {
        rooms.map(ConversationID.room).map { conversation in
            if notifyWriteTails[conversation] == nil { directory.setStoredNotifyMode(nil, for: conversation) }
            return Task { @MainActor [self] in
                try? await afterEarlierBookmarkWrites(to: conversation) { [self] in
                    let settings = try await port.fetchNotifySettings()
                    guard !isStopped, !directory.hasLeft(conversation.jid) else { return }
                    directory.setStoredNotifyMode(settings.modes[conversation], for: conversation)
                    directory.setShowsMessagesInPush(
                        settings.showingMessagesInPush.contains(conversation), for: conversation)
                }
            }
        }
    }

    func loadNotifyModes() async {
        guard let settings = try? await port.fetchNotifySettings() else { return }
        guard !isStopped, !Task.isCancelled else { return }
        directory.replaceNotifySettings(settings)
    }

    /// XEP-0492 notification mode for a conversation. Optimistic, rolled
    /// back if the server rejects it.
    public func setNotifyMode(_ mode: NotifyMode, for conversation: ConversationID) async throws {
        try refuseSettings(ofLeft: conversation)
        let previous = directory.notifyMode(for: conversation)
        directory.setNotifyMode(mode, for: conversation)
        try await storeNotifySettings(for: conversation, changing: .mode(mode)) {
            self.directory.setNotifyMode(previous, for: conversation)
        }
    }

    /// Mutes the conversation or restores its default mode when unmuting.
    public func toggleNotifyMute(for conversation: ConversationID) async throws {
        let next: NotifyMode = directory.notifyMode(for: conversation) == .never
            ? directory.defaultNotifyMode(for: conversation) : .never
        try await setNotifyMode(next, for: conversation)
    }

    /// Whether push notifications for a conversation show who wrote and
    /// what, on every device of the account. Optimistic, rolled back if
    /// the server rejects it.
    public func setShowsMessagesInPush(_ shows: Bool, for conversation: ConversationID) async throws {
        try refuseSettings(ofLeft: conversation)
        let previous = directory.showsMessagesInPush(for: conversation)
        directory.setShowsMessagesInPush(shows, for: conversation)
        try await storeNotifySettings(for: conversation, changing: .showsMessagesInPush(shows)) {
            self.directory.setShowsMessagesInPush(previous, for: conversation)
        }
    }

    /// A room the account left keeps its notifications off until it is
    /// joined again, so its settings take no change.
    private func refuseSettings(ofLeft conversation: ConversationID) throws {
        if conversation.isRoom, directory.hasLeft(conversation.jid) { throw PortError.invalidRequest }
    }

    private enum NotifySetting {
        case mode(NotifyMode)
        case showsMessagesInPush(Bool)
    }

    /// Sends both settings of a conversation, which the account stores
    /// together. Writes to one conversation run one at a time, so a later
    /// one never repeats a value an earlier one is about to roll back.
    /// The setting that is not changing is read from the server first: it
    /// may have changed on another device since this session loaded it.
    /// If that read fails nothing is written, since a stale value could
    /// undo the other device's change.
    private func storeNotifySettings(
        for conversation: ConversationID,
        changing changed: NotifySetting,
        rollback: @escaping @MainActor () -> Void
    ) async throws {
        try await afterEarlierBookmarkWrites(to: conversation) { [self] in
            do {
                // Left while this waited its turn, or while the read
                // below was out: nothing is stored. A mode stored now
                // would take the room out of the rooms the account left.
                try refuseSettings(ofLeft: conversation)
                let stored = try await port.fetchNotifySettings()
                try refuseSettings(ofLeft: conversation)
                // The new value is applied again: a write that ran in
                // between may have refreshed it away.
                switch changed {
                case let .mode(mode):
                    directory.setShowsMessagesInPush(
                        stored.showingMessagesInPush.contains(conversation), for: conversation)
                    directory.setNotifyMode(mode, for: conversation)
                case let .showsMessagesInPush(shows):
                    directory.setStoredNotifyMode(stored.modes[conversation], for: conversation)
                    directory.setShowsMessagesInPush(shows, for: conversation)
                }
                try await port.setNotifyMode(
                    directory.notifyMode(for: conversation),
                    showsMessagesInPush: directory.showsMessagesInPush(for: conversation),
                    for: conversation)
            } catch {
                rollback()
                if conversation.isRoom, directory.hasLeft(conversation.jid) {
                    directory.setNotifyMode(.never, for: conversation)
                }
                throw error
            }
        }
    }

    /// Runs `write` once earlier writes to the conversation's bookmark
    /// have settled. Each reads the bookmark and writes it back whole, so
    /// two at once would undo each other.
    func afterEarlierBookmarkWrites(
        to conversation: ConversationID,
        _ write: @escaping @MainActor () async throws -> Void
    ) async throws {
        let earlier = notifyWriteTails[conversation]
        let task = Task { @MainActor in
            _ = await earlier?.result
            try await write()
        }
        notifyWriteTails[conversation] = task
        defer { if notifyWriteTails[conversation] == task { notifyWriteTails[conversation] = nil } }
        try await task.value
    }

    /// Creates a channel room, joins it, and returns its conversation.
    public func createChannel(name: String, summary: String?) async throws -> ConversationID {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let localpart = RoomLocalpart.make(from: trimmed) else { throw PortError.invalidRequest }
        let room = try await port.createRoom(localpart: localpart, name: trimmed, summary: summary, nick: account.nick)
        // A room deleted earlier in the session exists again.
        destroyedRooms.remove(room)
        lookupsBeforeDestruction[room] = nil
        unconfirmedDeletions.remove(room)
        roomsOfDeletedSpaces.remove(room)
        calls.sealedGroupRooms.remove(room)
        upsertChannel(Channel(roomJID: room, name: trimmed, summary: summary, position: directory.channels.count))
        // create_room joined, configured and then left the room. Its
        // self-presence may already have marked the room joined while the
        // leave's echo is still in flight, so rejoin unconditionally: the
        // server handles the leave first, then this join.
        presence.markLeft(room)
        await ensureJoined(room)
        return .room(room)
    }

    /// Creates a group DM with `members` and returns its conversation.
    public func createGroupDM(name: String, members: [BareJID]) async throws -> ConversationID {
        let room = try await port.createGroupDM(name: name, members: members)
        upsertChannel(Channel(roomJID: room, name: name, isGroupDM: true))
        await ensureJoined(room)
        return .room(room)
    }

    /// Opens (or creates) the 1:1 conversation with `peer`.
    public func directConversation(with peer: BareJID) -> ConversationID {
        if !directory.directConversations.contains(where: { $0.peer == peer }) {
            directory.touchDirect(peer, at: Date(), preview: nil)
        }
        return .direct(peer)
    }

    public func members(of room: BareJID) async throws -> [RoomMember] {
        try await port.listMembers(of: room)
    }

    /// Lists the room's declared bots into `roomBots`; a failed query
    /// keeps the last list.
    public func refreshRoomBots(in room: BareJID) async {
        let ticket = roomBots.beginRefresh(in: room)
        guard let listed = try? await port.listRoomBots(in: room) else { return }
        roomBots.replace(listed, in: room, ticket: ticket)
    }

    /// A bot joins a room for one send and leaves, so a bot-hatted presence
    /// for a JID the room has not declared means the list is stale. The
    /// server records the room before the bot leaves, so its leave presence
    /// is the one a refresh sees the bot in.
    func refreshRoomBotsIfUndeclared(by wirePresence: WirePresence) {
        let room = wirePresence.from.bare
        guard wirePresence.isBot,
              let bot = wirePresence.occupant?.realJID?.bare,
              !roomBots.isDeclared(bot, in: room) else { return }
        Task { await refreshRoomBots(in: room) }
    }

    public func setAffiliation(_ affiliation: RoomAffiliation, of user: BareJID, in room: BareJID, reason: String? = nil) async throws {
        try await port.setAffiliation(affiliation, of: user, in: room, reason: reason)
    }

    public func kick(nick: String, from room: BareJID, reason: String? = nil) async throws {
        try await port.kick(nick: nick, from: room, reason: reason)
    }

    public func searchUsers(_ query: String) async throws -> [UserSearchResult] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }
        return try await port.searchUsers(trimmed)
    }

    /// XEP-0055 directory results for an `@query` typed in a room, so
    /// people with no presence there can be mentioned. Answers from the
    /// room's cache when this query was searched before; otherwise searches
    /// and remembers the result, which also makes those users addressable
    /// by username in the room's mention directory.
    public func searchMentionableUsers(_ query: String, in room: BareJID) async throws -> [UserSearchResult] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }
        if let cached = mentionSearches.cached(trimmed, in: room) { return cached }
        let found = try await port.searchUsers(trimmed)
        mentionSearches.record(found, for: trimmed, in: room)
        return found
    }

    /// Cached directory results for an `@query` in `room`, without searching.
    public func cachedMentionableUsers(_ query: String, in room: BareJID) -> [UserSearchResult]? {
        mentionSearches.cached(query, in: room)
    }

    /// Our own occupant in `room`, for permission checks. The room may have
    /// given us a nick other than the one we asked for.
    public func selfOccupant(in room: BareJID) -> Occupant? {
        presence.occupant(named: presence.selfNicks[room] ?? account.nick, in: room)
    }
}

/// What a look at the directory found.
struct DirectoryLookup: Equatable {
    /// The rooms it listed.
    let rooms: Set<BareJID>
    /// False when the account's bookmarks could not be read: then the
    /// rooms it left are those known from before, not found afresh.
    let readOwnBookmarks: Bool
}

/// Derives a room localpart from a display name: lowercase ASCII letters,
/// digits and dashes, as Waddle channel JIDs use.
public enum RoomLocalpart {
    public static func make(from name: String) -> String? {
        var result = ""
        var lastWasDash = false
        for scalar in name.lowercased().unicodeScalars {
            if ("a"..."z").contains(scalar) || ("0"..."9").contains(scalar) {
                result.unicodeScalars.append(scalar)
                lastWasDash = false
            } else if !lastWasDash, !result.isEmpty {
                result += "-"
                lastWasDash = true
            }
        }
        while result.hasSuffix("-") {
            result.removeLast()
        }
        let capped = String(result.prefix(64))
        return capped.isEmpty ? nil : capped
    }
}
