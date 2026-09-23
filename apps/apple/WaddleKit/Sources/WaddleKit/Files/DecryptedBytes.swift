import Foundation

/// Output of one cipher, before digest checks.
struct DecryptedBytes {
    let plaintext: Data
    /// Whether the cipher itself authenticated the plaintext (a GCM tag).
    let isAuthenticated: Bool
    /// Whether the ciphertext's layout was a guess: GCM without `<size/>`
    /// whose trailing bytes failed as a tag may be tagless, or tagged with
    /// a bad tag. Only a plaintext digest then vouches for the result.
    var isLayoutAmbiguous = false
}
