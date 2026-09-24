import Foundation

/// XEP-0245 `/me`. The body is sent as-is; a receiver matches the exact
/// string "/me " (case-sensitive, including the space) in the first four
/// characters of the body and presents "* <sender> <action>".
public enum MeAction {
    public static let prefix = "/me "

    /// The action phrase after "/me ", or nil when the body is not a
    /// `/me` command (`/meshrugs`, ` /me x`, `/ME x`, …).
    public static func parse(body: String) -> String? {
        let scalars = body.unicodeScalars
        guard scalars.starts(with: prefix.unicodeScalars) else { return nil }
        return String(Substring(scalars.dropFirst(prefix.unicodeScalars.count)))
    }

    /// "* actor action"; either part may be empty.
    public static func presentation(actor: String, action: String) -> String {
        (["*", actor, action].filter { !$0.isEmpty }).joined(separator: " ")
    }

    /// The preview line for a `/me` body, or nil for any other body.
    public static func presentation(ofBody body: String, actor: String) -> String? {
        guard let action = parse(body: body) else { return nil }
        return presentation(actor: actor, action: action.trimmingCharacters(in: .whitespacesAndNewlines))
    }
}
