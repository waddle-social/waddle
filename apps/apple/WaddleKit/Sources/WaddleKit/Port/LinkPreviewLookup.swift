import Foundation

/// The spelling of a URL in the draft, carried unchanged in the lookup IQ.
public struct LinkPreviewURL: RawRepresentable, Hashable, Sendable {
    public let rawValue: String

    public init(rawValue: String) { self.rawValue = rawValue }
}

public struct LinkPreviewRequest: Hashable, Sendable {
    public let url: LinkPreviewURL
    public let scopeJid: BareJID

    public init(url: LinkPreviewURL, scopeJid: BareJID) {
        self.url = url
        self.scopeJid = scopeJid
    }
}

public struct LinkPreviewToken: RawRepresentable, Hashable, Sendable {
    public let rawValue: String

    public init(rawValue: String) { self.rawValue = rawValue }
}

/// The composer lookup's result from the message service.
public enum LinkPreviewLookup: Hashable, Sendable {
    case ready(LinkPreviewPayload)
    case unsupported
    case blocked
    case failed
}

/// A card and the short-lived authorization to include it with a message.
public struct LinkPreviewPayload: Hashable, Sendable {
    public let token: LinkPreviewToken
    public let expiresAt: Date
    public let preview: LinkPreview

    public init(token: LinkPreviewToken, expiresAt: Date, preview: LinkPreview) {
        self.token = token
        self.expiresAt = expiresAt
        self.preview = preview
    }

    func ifFresh(at now: Date) -> Self? {
        expiresAt > now ? self : nil
    }
}
