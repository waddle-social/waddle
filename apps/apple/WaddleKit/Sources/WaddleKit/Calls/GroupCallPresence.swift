import Observation

struct GroupCallParticipantKey: Hashable {
    let nick: String
    let realJID: JID?
}

struct GroupCallPreparationKey: Hashable {
    let room: BareJID
    let participant: GroupCallParticipantKey
}

/// A preparing echo is registered before its presence can reach the room.
@MainActor public final class GroupCallPreparingWaiter {
    let key: GroupCallPreparationKey
    let result = GroupCallWaiter<Bool>()

    init(key: GroupCallPreparationKey) { self.key = key }
}

/// Room-call membership follows active Muji contents, never room membership alone.
@MainActor @Observable public final class GroupCallPresence {
    /// One entry per nick, in first-advertised order; the last identity owns the nick.
    public private(set) var participants: [BareJID: [GroupCallParticipant]] = [:]
    /// Active contents imply audio, even when every participant joins muted.
    public private(set) var media: [BareJID: CallMedia] = [:]
    @ObservationIgnored private var active: [BareJID: [GroupCallParticipant]] = [:]
    @ObservationIgnored private var preparing: [BareJID: Set<GroupCallParticipantKey>] = [:]
    @ObservationIgnored private var echoes: [GroupCallPreparationKey: GroupCallPreparingWaiter] = [:]
    @ObservationIgnored private var peers: [GroupCallPreparationKey: GroupCallWaiter<Bool>] = [:]

    public init() {}

    /// Preparation waiters fire after preparation changes, before the active view changes.
    public func apply(_ presence: WirePresence) {
        guard let nick = presence.from.resource else { return }
        switch presence.kind {
        case .available, .unavailable: break
        case .error, .other: return
        }
        let room = presence.from.bare
        let identity = GroupCallParticipantKey(nick: nick, realJID: presence.occupant?.realJID)
        let marker = presence.kind == .unavailable ? nil : presence.groupCall
        if marker?.isPreparing == true {
            preparing[room, default: []].insert(identity)
            let key = GroupCallPreparationKey(room: room, participant: identity)
            echoes.removeValue(forKey: key)?.result.resolve(true)
        } else {
            preparing[room] = preparing[room]?.filter { !matches($0, identity, includeAggregate: identity.realJID == nil) }
            if preparing[room]?.isEmpty == true { preparing[room] = nil }
        }
        for key in peers.keys.filter({ $0.room == room && !hasOtherPreparing($0) }) {
            peers.removeValue(forKey: key)?.resolve(true)
        }
        if let marker, let tracks = marker.media {
            let participant = GroupCallParticipant(nick: nick, realJID: identity.realJID, media: tracks,
                isHandRaised: marker.isHandRaised, isMuted: marker.isMuted)
            if let index = active[room]?.firstIndex(where: { $0.nick == nick && $0.realJID == identity.realJID }) {
                active[room]?[index] = participant
            } else {
                active[room, default: []].append(participant)
            }
            setFlags(in: room, nick: nick, raised: marker.isHandRaised, muted: marker.isMuted)
            publish(room)
        } else if marker?.isPreparing != true {
            removeActive(in: room, identity: identity, includeAggregate: identity.realJID == nil)
        }
    }

    /// A local leave drops its exact resource and an identity-less aggregate entry.
    public func clearParticipant(in room: BareJID, nick: String, realJID: JID? = nil) {
        removeActive(in: room, identity: .init(nick: nick, realJID: realJID), includeAggregate: true)
    }

    /// Only one echo waiter per participant exists; replacement cancels the previous one.
    public func registerPreparingEcho(in room: BareJID, nick: String, realJID: JID?) -> GroupCallPreparingWaiter {
        let key = GroupCallPreparationKey(room: room, participant: .init(nick: nick, realJID: realJID))
        let waiter = GroupCallPreparingWaiter(key: key)
        echoes.updateValue(waiter, forKey: key)?.result.cancel()
        return waiter
    }

    public func awaitPreparingEcho(_ waiter: GroupCallPreparingWaiter, timeout: Duration) async -> Bool {
        let completed = await waiter.result.wait(timeout: timeout) == true
        if echoes[waiter.key] === waiter { echoes[waiter.key] = nil }
        return completed
    }

    /// Same-nick sibling resources still count as other preparing participants.
    public func awaitNoOtherPreparing(in room: BareJID, nick: String, realJID: JID?, timeout: Duration) async -> Bool {
        let key = GroupCallPreparationKey(room: room, participant: .init(nick: nick, realJID: realJID))
        guard hasOtherPreparing(key) else { return !Task.isCancelled }
        let waiter = GroupCallWaiter<Bool>()
        peers.updateValue(waiter, forKey: key)?.cancel()
        let completed = await waiter.wait(timeout: timeout) == true
        if peers[key] === waiter { peers[key] = nil }
        return completed
    }

    public func cancelPreparationWaiters(in room: BareJID, nick: String) {
        for key in echoes.keys.filter({ $0.room == room && $0.participant.nick == nick }) {
            echoes.removeValue(forKey: key)?.result.cancel()
        }
        for key in peers.keys.filter({ $0.room == room && $0.participant.nick == nick }) {
            peers.removeValue(forKey: key)?.cancel()
        }
    }

    /// Stream loss forgets room advertisements and completes every pending waiter.
    public func clear() {
        for waiter in echoes.values { waiter.result.cancel() }
        for waiter in peers.values { waiter.cancel() }
        echoes.removeAll()
        peers.removeAll()
        preparing.removeAll()
        active.removeAll()
        participants.removeAll()
        media.removeAll()
    }

    private func hasOtherPreparing(_ key: GroupCallPreparationKey) -> Bool {
        preparing[key.room, default: []].contains { $0 != key.participant }
    }

    private func matches(_ key: GroupCallParticipantKey, _ target: GroupCallParticipantKey, includeAggregate: Bool) -> Bool {
        key.nick == target.nick && (target.realJID == nil || key.realJID == target.realJID || (includeAggregate && key.realJID == nil))
    }

    private func removeActive(in room: BareJID, identity: GroupCallParticipantKey, includeAggregate: Bool) {
        active[room]?.removeAll { matches(.init(nick: $0.nick, realJID: $0.realJID), identity, includeAggregate: includeAggregate) }
        setFlags(in: room, nick: identity.nick, raised: false, muted: false)
        publish(room)
    }

    private func setFlags(in room: BareJID, nick: String, raised: Bool, muted: Bool) {
        guard let entries = active[room] else { return }
        active[room] = entries.map {
            guard $0.nick == nick else { return $0 }
            return GroupCallParticipant(nick: $0.nick, realJID: $0.realJID, media: $0.media,
                isHandRaised: raised, isMuted: muted)
        }
    }

    private func publish(_ room: BareJID) {
        guard let entries = active[room], !entries.isEmpty else {
            active[room] = nil
            if participants[room] != nil { participants[room] = nil }
            if media[room] != nil { media[room] = nil }
            return
        }
        var projected: [GroupCallParticipant] = []
        for entry in entries {
            if let index = projected.firstIndex(where: { $0.nick == entry.nick }) {
                let previous = projected[index]
                projected[index] = GroupCallParticipant(nick: entry.nick, realJID: entry.realJID,
                    media: CallMedia(audio: previous.media.audio || entry.media.audio, video: previous.media.video || entry.media.video),
                    isHandRaised: entry.isHandRaised, isMuted: entry.isMuted)
            } else {
                projected.append(entry)
            }
        }
        if participants[room] != projected { participants[room] = projected }
        let aggregate = CallMedia(audio: true, video: entries.contains { $0.media.video })
        if media[room] != aggregate { media[room] = aggregate }
    }
}
