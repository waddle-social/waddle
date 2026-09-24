import Foundation

/// Why a command stage was refused before it reached the wire.
public enum ExtensionCommandSubmitError: Error, Sendable, Equatable {
    /// The form asks for a secret; it is never submitted forward.
    case forbiddenField(variable: String)
    /// Visible required fields without a value.
    case missingRequiredFields(variables: [String])
}
