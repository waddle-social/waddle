import Foundation

/// The time-sensitive label on a timed occurrence.
public enum EventStatus: Equatable, Sendable {
    case startingSoon
    case happeningNow

    public var label: String {
        switch self {
        case .startingSoon: "Starting soon"
        case .happeningNow: "Happening now"
        }
    }
}

extension EventOccurrence {
    public func status(at now: Date) -> EventStatus? {
        guard case let .instant(start) = start else { return nil }
        let finish: Date
        if case let .instant(end)? = end {
            finish = end
        } else {
            finish = start.addingTimeInterval(60 * 60)
        }
        guard now < finish, start.timeIntervalSince(now) <= 2 * 60 * 60 else { return nil }
        return now < start ? .startingSoon : .happeningNow
    }
}
