import Foundation
import SwiftUI

enum AppThemePreference: String, CaseIterable, Identifiable {
    case system
    case dark
    case light

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system:
            return "System"
        case .dark:
            return "Dark"
        case .light:
            return "Light"
        }
    }

    var preferredColorScheme: ColorScheme? {
        switch self {
        case .system:
            return nil
        case .dark:
            return .dark
        case .light:
            return .light
        }
    }
}

enum ChatScrollDirection: String, CaseIterable, Identifiable {
    case chat
    case social

    var id: String { rawValue }

    var title: String {
        switch self {
        case .chat:
            return "Chat"
        case .social:
            return "Social"
        }
    }

    var description: String {
        switch self {
        case .chat:
            return "Regular chat layout with newer messages at the bottom."
        case .social:
            return "Feed layout with newer messages at the top."
        }
    }
}

enum AppConfig {
    private static let activeServerURLKey = "waddle.apple.active-server"
    private static let sessionMapKey = "waddle.apple.session-map"
    static let themePreferenceKey = "waddle.apple.theme-preference"
    static let scrollDirectionKey = "waddle.apple.scroll-direction"
    static let mobileShellTabKey = "waddle.apple.mobile-shell-tab"

    static let defaultServerURL = URL(string: "https://xmpp.waddle.social")!
#if os(macOS)
    static let desktopWindowWidth: CGFloat = 1_420
    static let desktopWindowHeight: CGFloat = 920
    static let desktopSidebarMinWidth: CGFloat = 280
    static let desktopSidebarIdealWidth: CGFloat = 324
    static let desktopSidebarMaxWidth: CGFloat = 380
    static let desktopPanelCornerRadius: CGFloat = 24
#endif

    static func normalizedServerURL(from value: String) -> URL? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let unescaped = trimmed
            .replacingOccurrences(of: "\\\"", with: "\"")
            .trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
        guard !unescaped.isEmpty else {
            return nil
        }

        let candidate = unescaped.contains("://") ? unescaped : "https://\(unescaped)"
        guard var components = URLComponents(string: candidate),
              let scheme = components.scheme?.lowercased(),
              (scheme == "https" || scheme == "http"),
              components.host != nil else {
            return nil
        }

        components.path = ""
        components.query = nil
        components.fragment = nil
        return components.url
    }

    static var persistedServerURL: URL {
        let stored = UserDefaults.standard.string(forKey: activeServerURLKey) ?? defaultServerURL.absoluteString
        return normalizedServerURL(from: stored) ?? defaultServerURL
    }

    static func saveServerURL(_ url: URL) {
        UserDefaults.standard.set(url.absoluteString, forKey: activeServerURLKey)
    }

    static func storedSessionID(for serverURL: URL) -> String? {
        readSessionMap()[serverKey(for: serverURL)]
    }

    static func saveSessionID(_ sessionID: String, for serverURL: URL) {
        var map = readSessionMap()
        map[serverKey(for: serverURL)] = sessionID
        writeSessionMap(map)
    }

    static func clearSessionID(for serverURL: URL) {
        var map = readSessionMap()
        map.removeValue(forKey: serverKey(for: serverURL))
        writeSessionMap(map)
    }

    private static func serverKey(for serverURL: URL) -> String {
        normalizedServerURL(from: serverURL.absoluteString)?.absoluteString ?? serverURL.absoluteString
    }

    private static func readSessionMap() -> [String: String] {
        guard let raw = UserDefaults.standard.dictionary(forKey: sessionMapKey) else {
            return [:]
        }

        var output: [String: String] = [:]
        for (key, value) in raw {
            if let session = value as? String {
                output[key] = session
            }
        }
        return output
    }

    private static func writeSessionMap(_ map: [String: String]) {
        UserDefaults.standard.set(map, forKey: sessionMapKey)
    }
}
