import Foundation
import Observation

/// `urn:waddle:in-call:0`: an emoji someone in a call sends for everyone
/// in it to see for a moment. Never stored, never archived.
public struct InCallReaction: Hashable, Sendable {
    public var sid: String
    public var emoji: String

    public init(sid: String, emoji: String) {
        self.sid = sid
        self.emoji = emoji
    }
}

/// One reaction on the stage, rising and fading.
public struct CallReactionBurst: Identifiable, Equatable, Sendable {
    public let id: Int
    public let emoji: String
    /// Whose it is, as the call's surfaces name people; `nil` for our own.
    public let from: JID?
    /// Where across the stage it rises, 0 to 1: its own place, so two at once do not sit on each other.
    public let lane: Double
    public let expires: Date

    /// "ada reacted 👍", "You reacted 👍": a screen reader says the emoji's own name.
    ///
    /// - Parameter ownNick: Our nick in the room of a room's call, which
    ///   hands our own reaction back to us as it does everyone's; `nil` in a call of two.
    public func spoken(ownNick: String?) -> String {
        guard let from else { return "You reacted \(emoji)" }
        guard let ownNick else { return "\(from.bare.localpart ?? "Someone") reacted \(emoji)" }
        // In a room the sender is an occupant, named by their nick.
        guard let nick = from.resource else { return "Someone reacted \(emoji)" }
        return nick == ownNick ? "You reacted \(emoji)" : "\(nick) reacted \(emoji)"
    }
}

/// The reactions showing on a call's stage (web `in-call-reactions.ts`).
public struct CallReactionLedger: Equatable, Sendable {
    /// How long one shows (web: 2400 ms).
    public static let lifetime: TimeInterval = 2.4
    /// Shown at once. One more waits for nothing: it is not shown.
    public static let limit = 8
    /// Shown at once from one sender, so that nobody fills the stage alone.
    public static let limitEach = 3
    /// The longest emoji there is has about ten scalars; a "single
    /// character" can be made of hundreds.
    static let longestEmoji = 12
    /// The six the picker offers, as on the web.
    public static let offered = ["👍", "❤️", "😂", "🔥", "👏", "🎉"]

    public private(set) var bursts: [CallReactionBurst] = []
    private var sequence = 0

    public init() { /* nothing showing */ }

    /// Adds one. Anything that is not a single emoji is not shown: the
    /// wire carries whatever the sender typed.
    ///
    /// One past the limits is dropped, not shown in an older one's place.
    /// In a room anyone in it can send these as fast as they like, and each
    /// one shown is work on the thread that draws the call's video: what
    /// is on the stage changes at most `limit` times in `lifetime`.
    public mutating func received(_ emoji: String, from: JID?, at now: Date) {
        guard Self.isOneEmoji(emoji) else { return }
        expire(at: now)
        guard bursts.count < Self.limit, bursts.count(where: { $0.from == from }) < Self.limitEach else { return }
        sequence += 1
        bursts.append(CallReactionBurst(
            id: sequence, emoji: emoji, from: from, lane: Self.lane(of: sequence),
            expires: now.addingTimeInterval(Self.lifetime)
        ))
    }

    public mutating func expire(at now: Date) {
        bursts.removeAll { $0.expires <= now }
    }

    public mutating func clear() {
        bursts = []
    }

    /// When the next one is due to go, for whoever is waiting to take it away.
    public var nextExpiry: Date? { bursts.map(\.expires).min() }

    /// Spread by the golden ratio, so neighbours in time are apart in place.
    static func lane(of sequence: Int) -> Double {
        (Double(sequence) * 0.618_033_988_75).truncatingRemainder(dividingBy: 1)
    }

    /// One character that is an emoji, with whatever joiners and variation
    /// selectors it is made of.
    public static func isOneEmoji(_ text: String) -> Bool {
        let scalars = text.unicodeScalars
        guard text.count == 1, scalars.count <= longestEmoji, let first = scalars.first else { return false }
        if first.properties.isEmojiPresentation { return true }
        // A digit, "#" or "✌" is an emoji only when the text presents it as
        // one: with the emoji selector, or with a skin tone.
        return first.properties.isEmoji && scalars.dropFirst().contains { $0.value == 0xFE0F || $0.properties.isEmojiModifier }
    }

    /// Whether a reaction that arrived belongs to the call in the slot.
    ///
    /// A call of two shares one session id, and the reaction comes from the
    /// device we are in the call with. A room's call has no id the room
    /// shares, each device making its own, so there it is the room it came
    /// through that says which call it is for.
    public static func belongs(_ reaction: InCallReaction, from: JID, isGroupchat: Bool, to state: CallState) -> Bool {
        switch state {
        case .active(let call):
            !isGroupchat && from == call.peer && reaction.sid == call.sid
        case .inGroup(let call):
            isGroupchat && from.bare == call.room && from.resource != nil
        case .idle, .incoming, .outgoing, .joiningGroup, .ended:
            false
        }
    }

    /// Where a reaction to the call in the slot is sent.
    public enum Destination: Hashable, Sendable {
        /// The device we are in a call of two with.
        case device(JID, sid: String)
        case room(BareJID, sid: String)
    }

    public static func destination(in state: CallState) -> Destination? {
        switch state {
        case .active(let call): .device(call.peer, sid: call.sid)
        case .inGroup(let call): .room(call.room, sid: call.sid)
        case .idle, .incoming, .outgoing, .joiningGroup, .ended: nil
        }
    }
}

/// The reactions on the stage of the session's call, for whoever draws them.
@MainActor @Observable public final class CallReactions {
    public private(set) var bursts: [CallReactionBurst] = []
    @ObservationIgnored private var ledger = CallReactionLedger()
    @ObservationIgnored private var sweep: Task<Void, Never>?

    public init() { /* nothing showing */ }

    func received(_ emoji: String, from: JID?, at now: Date = Date()) {
        ledger.received(emoji, from: from, at: now)
        publish()
    }

    func clear() {
        ledger.clear()
        publish()
    }

    /// Takes each away when its time is up, without a clock ticking while none shows.
    private func publish() {
        if bursts != ledger.bursts { bursts = ledger.bursts }
        sweep?.cancel()
        guard let due = ledger.nextExpiry else { return }
        sweep = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(0, due.timeIntervalSinceNow)))
            guard !Task.isCancelled, let self else { return }
            self.ledger.expire(at: Date())
            self.publish()
        }
    }
}
