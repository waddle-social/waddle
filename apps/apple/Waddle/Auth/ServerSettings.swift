import Foundation

/// The server the app signs in to, and this install's XMPP resource.
enum ServerSettings {
    static let defaultServer = URL(string: "https://xmpp.waddle.social")!

    private static let serverKey = "waddle.apple.server"
    private static let resourceKey = "waddle.apple.resource"

    static var current: URL {
        UserDefaults.standard.string(forKey: serverKey).flatMap(normalized(from:)) ?? defaultServer
    }

    static func save(_ server: URL) {
        UserDefaults.standard.set(server.absoluteString, forKey: serverKey)
    }

    /// `https://host[:port]` from user input, or nil when it isn't a URL.
    static func normalized(from input: String) -> URL? {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let candidate = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
        guard var components = URLComponents(string: candidate),
              let scheme = components.scheme?.lowercased(),
              scheme == "https" || scheme == "http",
              components.host?.isEmpty == false
        else { return nil }
        components.path = ""
        components.query = nil
        components.fragment = nil
        return components.url
    }

    /// A random resource per install. Full JIDs are visible to contacts and
    /// room occupants, so nothing credential-derived may appear here.
    static var resource: String {
        if let stored = UserDefaults.standard.string(forKey: resourceKey) {
            return stored
        }
        let created = "waddle-\(platformTag)-\(UUID().uuidString.prefix(8).lowercased())"
        UserDefaults.standard.set(created, forKey: resourceKey)
        return created
    }

    private static var platformTag: String {
        #if os(macOS)
        return "mac"
        #else
        return "ios"
        #endif
    }
}
