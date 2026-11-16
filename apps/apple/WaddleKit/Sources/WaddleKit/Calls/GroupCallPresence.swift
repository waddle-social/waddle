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
    /// Who was already preparing when the room echoed a participant's own
    /// preparation. XEP-0272 §Joining has a joiner wait for those, not for
    /// whoever starts preparing afterwards: they in turn wait for the joiner.
    @ObservationIgnored private var awaited: [GroupCallPreparationKey: Set<GroupCallParticipantKey>] = [:]

    public init() {}

    /// Preparation waiters fire after preparation changes, before the active view changes.
    public func apply(_ presence: WirePresence) {
        guard let nick = presence.from.resource else { return }
        // An error from an occupant's address means they are not in the
        // room; subscription traffic says nothing about a call.
        switch presence.kind {
        case .available, .unavailable, .error: break
        case .other: return
        }
        let room = presence.from.bare
        let identity = GroupCallParticipantKey(nick: nick, realJID: presence.occupant?.realJID)
        let marker = presence.kind == .available ? presence.groupCall : nil
        if marker?.isPreparing == true {
            let key = GroupCallPreparationKey(room: room, participant: identity)
            if let waiter = echoes.removeValue(forKey: key) {
                awaited[key] = preparing[room, default: []].subtracting([identity])
                waiter.result.resolve(true)
            }
            preparing[room, default: []].insert(identity)
        } else {
            preparing[room] = preparing[room]?.filter { !matches($0, identity, includeAggregate: identity.realJID == nil) }
            if preparing[room]?.isEmpty == true { preparing[room] = nil }
            // Whoever finished is no longer waited for, even if they prepare again.
            let stillPreparing = preparing[room] ?? []
            for key in awaited.keys where key.room == room { awaited[key]?.formIntersection(stillPreparing) }
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
        awaited[key] = nil
        return waiter
    }

    public func awaitPreparingEcho(_ waiter: GroupCallPreparingWaiter, timeout: Duration) async -> Bool {
        let completed = await waiter.result.wait(timeout: timeout) == true
        if echoes[waiter.key] === waiter { echoes[waiter.key] = nil }
        return completed
    }

    /// Waits for the participants who were preparing when the room echoed
    /// ours. Same-nick sibling resources count as other participants.
    public func awaitNoOtherPreparing(in room: BareJID, nick: String, realJID: JID?, timeout: Duration) async -> Bool {
        let key = GroupCallPreparationKey(room: room, participant: .init(nick: nick, realJID: realJID))
        guard hasOtherPreparing(key) else {
            awaited[key] = nil
            return !Task.isCancelled
        }
        let waiter = GroupCallWaiter<Bool>()
        peers.updateValue(waiter, forKey: key)?.cancel()
        let completed = await waiter.wait(timeout: timeout) == true
        if peers[key] === waiter {
            peers[key] = nil
            awaited[key] = nil
        }
        return completed
    }

    public func cancelPreparationWaiters(in room: BareJID, nick: String) {
        for key in echoes.keys.filter({ $0.room == room && $0.participant.nick == nick }) {
            echoes.removeValue(forKey: key)?.result.cancel()
        }
        for key in peers.keys.filter({ $0.room == room && $0.participant.nick == nick }) {
            peers.removeValue(forKey: key)?.cancel()
        }
        for key in awaited.keys where key.room == room && key.participant.nick == nick { awaited[key] = nil }
    }

    /// This client is no longer in `room`: it hears nothing more about the
    /// room's call, and a rejoin replays only who is there then. Forgetting
    /// the room keeps a stale preparing entry from stalling every later join.
    public func clearRoom(_ room: BareJID) {
        for key in echoes.keys.filter({ $0.room == room }) { echoes.removeValue(forKey: key)?.result.cancel() }
        for key in peers.keys.filter({ $0.room == room }) { peers.removeValue(forKey: key)?.cancel() }
        for key in awaited.keys where key.room == room { awaited[key] = nil }
        preparing[room] = nil
        active[room] = nil
        publish(room)
    }

    /// Stream loss forgets room advertisements and completes every pending waiter.
    public func clear() {
        for waiter in echoes.values { waiter.result.cancel() }
        for waiter in peers.values { waiter.cancel() }
        echoes.removeAll()
        peers.removeAll()
        awaited.removeAll()
        preparing.removeAll()
        active.removeAll()
        participants.removeAll()
        media.removeAll()
    }

    private func hasOtherPreparing(_ key: GroupCallPreparationKey) -> Bool {
        let current = preparing[key.room, default: []]
        if let earlier = awaited[key] { return !earlier.isDisjoint(with: current) }
        // No echo was seen for this participant: everyone else preparing counts.
        return current.contains { $0 != key.participant }
    }

    private func matches(_ key: GroupCallParticipantKey, _ target: GroupCallParticipantKey, includeAggregate: Bool) -> Bool {
        key.nick == target.nick && (target.realJID == nil || key.realJID == target.realJID || (includeAggregate && key.realJID == nil))
    }

    private func removeActive(in room: BareJID, identity: GroupCallParticipantKey, includeAggregate: Bool) {
        active[room]?.removeAll { matches(.init(nick: $0.nick, realJID: $0.realJID), identity, includeAggregate: includeAggregate) }
        publish(room)
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
                // One row per nick: each device keeps its own flags, and the
                // row has a hand up if any does and is muted only if all are.
                projected[index] = GroupCallParticipant(nick: entry.nick, realJID: entry.realJID,
                    media: CallMedia(audio: previous.media.audio || entry.media.audio, video: previous.media.video || entry.media.video),
                    isHandRaised: previous.isHandRaised || entry.isHandRaised, isMuted: previous.isMuted && entry.isMuted)
            } else {
                projected.append(entry)
            }
        }
        if participants[room] != projected { participants[room] = projected }
        let aggregate = CallMedia(audio: true, video: entries.contains { $0.media.video })
        if media[room] != aggregate { media[room] = aggregate }
    }
}
