import Foundation

enum AppConfig {
    private static let activeServerURLKey = "waddle.apple.active-server"
    private static let sessionMapKey = "waddle.apple.session-map"

    static let defaultServerURL = URL(string: "https://xmpp.waddle.social")!

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
