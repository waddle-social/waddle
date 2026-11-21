import Foundation
import Security

/// Keeps the session credential in the Keychain, one item per server.
/// The session id is an XMPP bearer token; it never goes to UserDefaults.
enum CredentialStore {
    private static let service = "social.waddle.session"

    enum SessionIDRead: Equatable {
        case stored(String)
        case missing
        case unavailable
    }

    static func sessionID(for server: URL) -> String? {
        guard case let .stored(value) = readSessionID(for: server) else { return nil }
        return value
    }

    static func readSessionID(for server: URL) -> SessionIDRead {
        var query = baseQuery(for: server)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        return sessionIDRead(status: status, data: result as? Data)
    }

    /// After a reboot, AfterFirstUnlock items exist but cannot be read yet.
    /// Only a missing item is evidence that the person is signed out.
    static func sessionIDRead(status: OSStatus, data: Data?) -> SessionIDRead {
        if status == errSecItemNotFound { return .missing }
        guard status == errSecSuccess, let data,
              let value = String(data: data, encoding: .utf8), !value.isEmpty
        else { return .unavailable }
        return .stored(value)
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
