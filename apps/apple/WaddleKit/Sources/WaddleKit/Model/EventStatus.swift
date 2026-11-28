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
    static let missingTimedEndDuration: TimeInterval = 60 * 60

    /// The end used for live status and the seven-day list.
    var displayEnd: CalendarMoment? {
        guard case let .instant(start) = start else { return end }
        if case .instant? = end { return end }
        return .instant(start.addingTimeInterval(Self.missingTimedEndDuration))
    }

    public func status(at now: Date) -> EventStatus? {
        guard case let .instant(start) = start, case let .instant(finish)? = displayEnd else { return nil }
        guard now < finish, start.timeIntervalSince(now) <= 2 * 60 * 60 else { return nil }
        return now < start ? .startingSoon : .happeningNow
    }
}
