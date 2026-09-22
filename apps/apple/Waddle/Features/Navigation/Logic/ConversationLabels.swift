import Foundation
import WaddleKit

/// SF Symbol names for room kinds.
enum ChannelSymbol {
    static func name(for channel: Channel) -> String {
        if channel.isGroupDM { return "person.2" }
        return name(for: channel.kind)
    }

    static func name(for kind: Channel.Kind) -> String {
        switch kind {
        case .text, .other: return "number"
        case .forum: return "text.bubble"
        case .voice: return "speaker.wave.2"
        }
    }
}

/// XEP-0492 notification mode copy.
enum NotifyModeLabels {
    /// Menu and picker order.
    static let ordered: [NotifyMode] = [.always, .onMention, .never]

    static func title(_ mode: NotifyMode) -> String {
        switch mode {
        case .always: return "All messages"
        case .onMention: return "Mentions only"
        case .never: return "Nothing"
        }
    }

    static func symbol(_ mode: NotifyMode) -> String {
        switch mode {
        case .always: return "bell"
        case .onMention: return "at"
        case .never: return "bell.slash"
        }
    }

    /// The swipe "mute" toggle: muting sets `.never`; unmuting restores
    /// the default for the conversation kind (mentions for rooms, all for
    /// DMs), matching `DirectoryStore.notifyMode(for:)`.
    static func toggledMute(_ current: NotifyMode, isRoom: Bool) -> NotifyMode {
        guard current == .never else { return .never }
        return isRoom ? .onMention : .always
    }
}

/// RFC 6121 availability copy.
enum AvailabilityTitle {
    static func title(_ availability: Availability) -> String {
        switch availability {
        case .available: return "Online"
        case .chat: return "Free to chat"
        case .away: return "Away"
        case .extendedAway: return "Away for a while"
        case .doNotDisturb: return "Do not disturb"
        case .offline: return "Offline"
        }
    }
}

/// Messages-style timestamps for list rows: time today, "Yesterday",
/// weekday within the week, else a short date.
enum ListTimestamp {
    static func string(for date: Date, now: Date = Date(), calendar: Calendar = .current) -> String {
        if calendar.isDate(date, inSameDayAs: now) {
            return date.formatted(date: .omitted, time: .shortened)
        }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: yesterday) {
            return "Yesterday"
        }
        let startOfToday = calendar.startOfDay(for: now)
        if let weekAgo = calendar.date(byAdding: .day, value: -6, to: startOfToday), date >= weekAgo, date < now {
            return date.formatted(.dateTime.weekday(.wide))
        }
        return date.formatted(date: .numeric, time: .omitted)
    }
}

/// Short, plain copy for a failed XMPP action.
enum ActionErrorCopy {
    static func message(for error: Error, fallback: String) -> String {
        guard let port = error as? PortError else { return fallback }
        switch port {
        case .notConnected: return "You're offline. Try again when you're connected."
        case .timeout: return "The server took too long to answer. Try again."
        case .rejected: return "The server declined this request."
        case .invalidRequest: return "That doesn't look right. Check it and try again."
        case .failed: return fallback
        }
    }
}
