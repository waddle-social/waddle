import Foundation

/// The `/shrug` kaomoji.
public enum Shrug {
    public static let text = "¯\\_(ツ)_/¯"

    /// `message ¯\_(ツ)_/¯`, or the shrug alone for an empty message.
    public static func appended(to message: String) -> String {
        message.isEmpty ? text : message + " " + text
    }
}
