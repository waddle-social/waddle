import Foundation

/// A bare XMPP address (`local@domain` or `domain`), RFC 7622 §3.
///
/// The localpart and domainpart are case-folded so equality matches the
/// server's routing (nodeprep/nameprep fold case); the resourcepart is not
/// part of a bare JID. Construct through `init?(parsing:)` so every value in
/// the app is known-valid.
public struct BareJID: Hashable, Sendable, Comparable, CustomStringConvertible {
    public let localpart: String?
    public let domain: String

    public init?(localpart: String?, domain: String) {
        let folded = domain.lowercased()
        guard Self.isValidDomain(folded) else { return nil }
        if let localpart {
            let local = localpart.lowercased()
            guard Self.isValidLocalpart(local) else { return nil }
            self.localpart = local
        } else {
            self.localpart = nil
        }
        self.domain = folded
    }

    /// Parses a bare JID. A full JID (`a@b/res`) is rejected: callers that
    /// hold a full JID must go through `JID(parsing:)?.bare` so dropping the
    /// resource is an explicit decision.
    public init?(parsing raw: String) {
        guard let jid = JID(parsing: raw), jid.resource == nil else { return nil }
        self = jid.bare
    }

    public var description: String {
        guard let localpart else { return domain }
        return "\(localpart)@\(domain)"
    }

    public static func < (lhs: BareJID, rhs: BareJID) -> Bool {
        lhs.description < rhs.description
    }

    /// Returns the full JID `self/resource`.
    public func with(resource: String) -> JID? {
        JID(bare: self, resource: resource)
    }

    private static func isValidLocalpart(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 1023
            && !value.contains(where: { "\"&'/:<>@".contains($0) || $0.isWhitespace })
    }

    private static func isValidDomain(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 1023
            && !value.contains(where: { "@/".contains($0) || $0.isWhitespace })
    }
}

/// An XMPP address with an optional resource (RFC 7622). MUC occupant JIDs
/// are `room@service/nick`; 1:1 peers are `user@domain/device`.
public struct JID: Hashable, Sendable, CustomStringConvertible {
    public let bare: BareJID
    public let resource: String?

    public init?(bare: BareJID, resource: String?) {
        if let resource {
            guard !resource.isEmpty, resource.utf8.count <= 1023 else { return nil }
        }
        self.bare = bare
        self.resource = resource
    }

    /// Parses `[local@]domain[/resource]`. The first `/` starts the resource
    /// (which may itself contain `/` and `@`); the first `@` before it ends
    /// the localpart.
    public init?(parsing raw: String) {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let addressPart: Substring
        let resource: String?
        if let slash = trimmed.firstIndex(of: "/") {
            addressPart = trimmed[..<slash]
            resource = String(trimmed[trimmed.index(after: slash)...])
        } else {
            addressPart = Substring(trimmed)
            resource = nil
        }
        let localpart: String?
        let domain: Substring
        if let at = addressPart.firstIndex(of: "@") {
            localpart = String(addressPart[..<at])
            domain = addressPart[addressPart.index(after: at)...]
        } else {
            localpart = nil
            domain = addressPart
        }
        guard let bare = BareJID(localpart: localpart, domain: String(domain)) else { return nil }
        self.init(bare: bare, resource: resource)
    }

    public var description: String {
        guard let resource else { return bare.description }
        return "\(bare)/\(resource)"
    }
}
