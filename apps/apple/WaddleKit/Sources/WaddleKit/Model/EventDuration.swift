import Foundation

/// The duration choices for a timed event. Presets run for elapsed minutes,
/// including across a clock change. A custom end is a fixed time.
public enum EventDuration: CaseIterable, Hashable, Sendable {
    case thirtyMinutes, oneHour, ninetyMinutes, twoHours, threeHours, custom

    public var label: String {
        switch self {
        case .thirtyMinutes: "30 minutes"
        case .oneHour: "1 hour"
        case .ninetyMinutes: "90 minutes"
        case .twoHours: "2 hours"
        case .threeHours: "3 hours"
        case .custom: "Custom"
        }
    }

    init(start: Date, end: Date?) {
        guard let end else {
            self = .thirtyMinutes
            return
        }
        let minutes = (end.timeIntervalSince(start) / 60).rounded()
        self = Self.allCases.first { $0.minutes == minutes } ?? .custom
    }

    func end(startingAt start: Date, customEnd: Date?) -> Date? {
        guard let minutes else { return customEnd }
        return start.addingTimeInterval(minutes * 60)
    }

    private var minutes: TimeInterval? {
        switch self {
        case .thirtyMinutes: 30
        case .oneHour: 60
        case .ninetyMinutes: 90
        case .twoHours: 120
        case .threeHours: 180
        case .custom: nil
        }
    }
}
