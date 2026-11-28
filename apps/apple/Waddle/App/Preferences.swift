import Foundation
import Observation
import SwiftUI
import WaddleKit

/// User preferences persisted in UserDefaults.
@MainActor
@Observable
final class Preferences {
    enum Appearance: String, CaseIterable, Identifiable {
        case system
        case light
        case dark

        var id: String { rawValue }

        var title: String {
            switch self {
            case .system: return "System"
            case .light: return "Daylight"
            case .dark: return "Night"
            }
        }

        /// As Settings lists them (web `settings`): System, Night, Daylight.
        static let settingsOrder: [Appearance] = [.system, .dark, .light]

        /// Android `settings` segment glyphs.
        var symbol: String {
            switch self {
            case .system: return "circle.lefthalf.filled"
            case .light: return "sun.max"
            case .dark: return "moon"
            }
        }

        var colorScheme: ColorScheme? {
            switch self {
            case .system: return nil
            case .light: return .light
            case .dark: return .dark
            }
        }
    }

    /// Where the newest message sits. `social` flips the timeline so the
    /// newest message leads and the composer sits above it, as the web
    /// client's "Newest at top" scroll direction does.
    enum MessageOrder: String, CaseIterable, Identifiable {
        case chat
        case social

        var id: String { rawValue }

        var title: String {
            switch self {
            case .chat: return "Chat"
            case .social: return "Social"
            }
        }

        var subtitle: String {
            switch self {
            case .chat: return "Newest at bottom"
            case .social: return "Newest at top"
            }
        }

        var isNewestFirst: Bool { self == .social }
    }

    var appearance: Appearance {
        didSet { defaults.set(appearance.rawValue, forKey: Keys.appearance) }
    }

    /// XEP-0085 chat states (typing indicators). XEP-0085 requires the
    /// user be able to disable them.
    var sendsChatStates: Bool {
        didSet { defaults.set(sendsChatStates, forKey: Keys.chatStates) }
    }

    /// XEP-0333 displayed markers. Cross-device read sync (XEP-0490) is
    /// independent of this and always on.
    var sendsReadReceipts: Bool {
        didSet { defaults.set(sendsReadReceipts, forKey: Keys.readReceipts) }
    }

    /// Show message previews in notifications.
    var showsNotificationPreviews: Bool {
        didSet { defaults.set(showsNotificationPreviews, forKey: Keys.previews) }
    }

    let messageSounds: MessageSoundPreference

    /// Whether this install registers with Apple's push service. Off
    /// means it is only notified while Waddle is running.
    var receivesPush: Bool {
        didSet {
            defaults.set(receivesPush, forKey: Keys.push)
            if receivesPush != oldValue { onReceivesPushChanged?() }
        }
    }

    /// Set by the owner of the push registrations.
    @ObservationIgnored var onReceivesPushChanged: (() -> Void)?

    /// Denser rows: smaller avatars and tighter spacing.
    var compactMessages: Bool {
        didSet { defaults.set(compactMessages, forKey: Keys.compact) }
    }

    var messageOrder: MessageOrder {
        didSet { defaults.set(messageOrder.rawValue, forKey: Keys.messageOrder) }
    }

    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        messageSounds = MessageSoundPreference(defaults: defaults)
        appearance = defaults.string(forKey: Keys.appearance).flatMap(Appearance.init(rawValue:)) ?? .system
        sendsChatStates = defaults.object(forKey: Keys.chatStates) as? Bool ?? true
        sendsReadReceipts = defaults.object(forKey: Keys.readReceipts) as? Bool ?? true
        showsNotificationPreviews = defaults.object(forKey: Keys.previews) as? Bool ?? true
        receivesPush = defaults.object(forKey: Keys.push) as? Bool ?? true
        compactMessages = defaults.object(forKey: Keys.compact) as? Bool ?? false
        messageOrder = defaults.string(forKey: Keys.messageOrder).flatMap(MessageOrder.init(rawValue:)) ?? .chat
    }

    private enum Keys {
        static let appearance = "waddle.apple.appearance"
        static let chatStates = "waddle.apple.chat-states"
        static let readReceipts = "waddle.apple.read-receipts"
        static let previews = "waddle.apple.notification-previews"
        static let push = "waddle.apple.receives-push"
        static let compact = "waddle.apple.compact-messages"
        static let messageOrder = "waddle.apple.message-order"
    }
}
