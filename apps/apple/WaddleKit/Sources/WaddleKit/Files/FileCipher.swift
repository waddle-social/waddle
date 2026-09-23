/// XEP-0448 §5 cipher, keyed by its namespace.
public enum FileCipher: String, Hashable, Sendable, CaseIterable, Codable {
    case aes128GCM = "urn:xmpp:ciphers:aes-128-gcm-nopadding:0"
    case aes256GCM = "urn:xmpp:ciphers:aes-256-gcm-nopadding:0"
    case aes256CBC = "urn:xmpp:ciphers:aes-256-cbc-pkcs7:0"

    public var keyByteCount: Int {
        switch self {
        case .aes128GCM: return 16
        case .aes256GCM, .aes256CBC: return 32
        }
    }

    public var ivByteCount: Int {
        switch self {
        case .aes128GCM, .aes256GCM: return 12
        case .aes256CBC: return 16
        }
    }
}
