/// A canonical XMPP domain used for media trust decisions.
public struct Host: Equatable, Sendable, CustomStringConvertible {
    public let description: String
    private let hasLabels: Bool

    public init(_ domain: String) {
        let folded = domain.lowercased()
        let canonical = folded.hasSuffix(".") ? String(folded.dropLast()) : folded
        description = canonical
        hasLabels = !canonical.isEmpty && !canonical.split(separator: ".", omittingEmptySubsequences: false).contains(where: \.isEmpty)
    }

    /// A person is at home only on the account's own domain. A name below it may be another
    /// server: tenants are named by domain, and one can sit under another's.
    func isHomeAccountHost(_ accountHost: Host) -> Bool {
        hasLabels && self == accountHost
    }

    /// A room is at home on the account's domain or on one of the two names the Waddle server
    /// gives its rooms and spaces under it. The app is not told which services the home server
    /// lists, so any other host is treated as another server's.
    func isHomeRoomHost(_ accountHost: Host) -> Bool {
        guard hasLabels, accountHost.hasLabels else { return false }
        return self == accountHost || Self.homeServiceLabels.contains { description == $0 + "." + accountHost.description }
    }

    private static let homeServiceLabels = ["muc", "spaces"]
}
