import Foundation
import Security

/// Keeps the session credential in the Keychain, one item per server.
/// The session id is an XMPP bearer token; it never goes to UserDefaults.
enum CredentialStore {
    private static let service = "social.waddle.session"

    static func sessionID(for server: URL) -> String? {
        var query = baseQuery(for: server)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data
        else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func save(_ sessionID: String, for server: URL) {
        let data = Data(sessionID.utf8)
        let query = baseQuery(for: server)
        let update: [String: Any] = [kSecValueData as String: data]
        if SecItemUpdate(query as CFDictionary, update as CFDictionary) == errSecItemNotFound {
            var insert = query
            insert[kSecValueData as String] = data
            insert[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            SecItemAdd(insert as CFDictionary, nil)
        }
    }

    static func remove(for server: URL) {
        SecItemDelete(baseQuery(for: server) as CFDictionary)
    }

    private static func baseQuery(for server: URL) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: server.absoluteString,
        ]
    }
}
