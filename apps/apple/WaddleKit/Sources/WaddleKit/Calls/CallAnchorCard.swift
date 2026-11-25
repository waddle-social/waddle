import Foundation

/// What a room's call announcement shows in its conversation (web
/// `CallAnchorCard`): a live call with a way in, or a call that is over
/// and how long it ran.
public struct CallAnchorCard: Equatable, Sendable {
    public enum Action: Equatable, Sendable {
        case join
        /// We are in some other call: this one cannot be joined until that one is left.
        case busy

        public var title: String {
            switch self {
            case .join: "Join"
            case .busy: "In another call"
            }
        }
    }

    public let isLive: Bool
    public let hasVideo: Bool
    /// "Live video call", "Call ended · 5m".
    public let title: String
    /// Who is in it while it is live; what the room said when it began, once it is over.
    public let summary: String
    /// How many are in it, while it is live.
    public let headcount: Int?
    /// `nil` when there is nothing to do: the call is over, or we are in it.
    public let action: Action?

    /// What the room knows of its call right now.
    public struct Live: Equatable, Sendable {
        /// Who is in it, by name; our own other device is "You".
        public var people: [String]
        public var hasVideo: Bool
        /// Our call slot holds this room's call.
        public var isOurs: Bool
        /// We are in some other call.
        public var isBusy: Bool
        /// A call can be joined in this room now (`canJoinGroupCall`).
        public var canJoin: Bool

        public init(people: [String], hasVideo: Bool, isOurs: Bool, isBusy: Bool, canJoin: Bool) {
            self.people = people
            self.hasVideo = hasVideo
            self.isOurs = isOurs
            self.isBusy = isBusy
            self.canJoin = canJoin
        }
    }

    /// - Parameters:
    ///   - said: What the room wrote when the call began ("ada started a call").
    ///   - ended: The room's word that it is over, when it has been read.
    ///   - live: The call going on in the room now, if any.
    ///   - isNewest: No later announcement is in the room's timeline. A room
    ///     has one call at a time, so only its latest announcement can be the
    ///     call going on now: an older one whose end was never read is over.
    public static func make(
        anchor: CallThreadAnchor, said: String, ended: CallThreadEnded?, live: Live?, isNewest: Bool
    ) -> CallAnchorCard {
        guard ended == nil, isNewest, let live else {
            let ran = ended?.duration.flatMap(durationText)
            return CallAnchorCard(
                isLive: false, hasVideo: anchor.hasVideo,
                title: ran.map { "Call ended · \($0)" } ?? "Call ended",
                summary: said, headcount: nil, action: nil
            )
        }
        let action: Action? = if live.isOurs { nil } else if live.canJoin { .join } else if live.isBusy { .busy } else { nil }
        return CallAnchorCard(
            isLive: true, hasVideo: live.hasVideo,
            title: live.hasVideo ? "Live video call" : "Live call",
            summary: peopleText(live.people), headcount: live.people.count, action: action
        )
    }

    /// "Ada, Linus, Grace +2"
    static func peopleText(_ people: [String], naming limit: Int = 3) -> String {
        guard !people.isEmpty else { return "No one connected" }
        let named = people.prefix(limit).joined(separator: ", ")
        return people.count > limit ? "\(named) +\(people.count - limit)" : named
    }

    /// "1h 2m", "1h", "5m", "42s" (web `formatCallThreadDuration`).
    public static func durationText(_ seconds: TimeInterval) -> String? {
        guard seconds.isFinite, seconds >= 0 else { return nil }
        let total = Int(seconds)
        let (hours, minutes) = (total / 3600, (total % 3600) / 60)
        if hours > 0 { return minutes > 0 ? "\(hours)h \(minutes)m" : "\(hours)h" }
        if minutes > 0 { return "\(minutes)m" }
        return "\(total)s"
    }

    /// The card in a sentence, for whoever cannot see it.
    public var spoken: String {
        guard isLive, let headcount else { return "\(title). \(summary)" }
        let people = headcount == 1 ? "1 person" : "\(headcount) people"
        let lead = action == .join ? "Join \(title.prefix(1).lowercased() + title.dropFirst())" : title
        return headcount == 0 ? "\(lead), no one connected" : "\(lead), \(people): \(summary)"
    }
}
