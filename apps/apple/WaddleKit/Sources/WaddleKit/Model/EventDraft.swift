import Foundation

/// An event as the editor saves it. Built only when the server would take
/// it, so Save can be disabled instead of the server refusing.
public struct EventDraft: Hashable, Sendable {
    /// Nil creates an event.
    public let eventID: String?
    public let title: String
    public let details: String?
    public let location: String?
    public let start: CalendarMoment?
    public let end: CalendarMoment?
    public let recurrence: EventRecurrence?
    public let workspaceRoom: BareJID?

    /// Trims the text and checks what the server checks. A rule equal to
    /// `storedRecurrence`, the one fetched, is handed back as it is: the
    /// server keeps a rule it already holds, even one this app would not
    /// write.
    public init?(
        eventID: String? = nil,
        title: String,
        details: String? = nil,
        location: String? = nil,
        start: CalendarMoment? = nil,
        end: CalendarMoment? = nil,
        recurrence: EventRecurrence? = nil,
        workspaceRoom: BareJID? = nil,
        storedRecurrence: EventRecurrence? = nil
    ) {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return nil }
        if let end {
            guard let start, end.isAfter(start) else { return nil }
        }
        if let recurrence {
            guard let start else { return nil }
            guard recurrence == storedRecurrence || Self.canWrite(recurrence, startingAt: start) else { return nil }
        }
        self.eventID = eventID
        self.title = title
        self.details = Self.trimmed(details)
        self.location = Self.trimmed(location)
        self.start = start
        self.end = end
        self.recurrence = recurrence
        self.workspaceRoom = workspaceRoom
    }

    /// The rules the server writes: every client shows them the same way.
    private static func canWrite(_ rule: EventRecurrence, startingAt start: CalendarMoment) -> Bool {
        guard rule.interval >= 1,
              rule.weekdays.isEmpty || rule.frequency == .weekly,
              rule.monthDays.isEmpty || rule.frequency == .monthly,
              rule.monthDays.allSatisfy({ (1...31).contains(abs($0)) })
        else { return false }
        switch (rule.end, start) {
        case (.never, _): return true
        case let (.count(count), _): return count >= 1
        // An all-day series cannot end on an instant.
        case (.until, .day): return false
        case let (.until(until), .instant(start)): return until >= start
        }
    }

    private static func trimmed(_ text: String?) -> String? {
        guard let text = text?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        return text
    }
}
