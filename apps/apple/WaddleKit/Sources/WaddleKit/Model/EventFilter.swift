/// Which events to show in the calendar.
public enum EventFilter: Hashable, Sendable {
    case all
    case goingOrMaybe

    /// Filters by the signed-in person's reply to the event or series.
    public func apply(to events: [CommunityEvent], for account: BareJID) -> [CommunityEvent] {
        switch self {
        case .all:
            return events
        case .goingOrMaybe:
            return events.filter { event in
                switch event.reply(of: account) {
                case .going, .maybe: true
                case .declined, nil: false
                }
            }
        }
    }
}
