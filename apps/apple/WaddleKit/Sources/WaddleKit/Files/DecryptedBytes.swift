import Foundation

/// Output of one cipher, before digest checks.
struct DecryptedBytes {
    let plaintext: Data
    /// Whether the cipher itself authenticated the plaintext (a GCM tag).
    let isAuthenticated: Bool
}
