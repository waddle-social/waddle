import Foundation

/// Admits only the clicked player document and sends explicit web links out of the card.
public struct LinkPreviewEmbedNavigation: Sendable {
    public enum Frame: Equatable, Sendable {
        case main, subframe, newWindow
    }

    public enum Decision: Equatable, Sendable {
        case allow, cancel, openExternally(URL)
    }

    private let initialURL: URL
    private var admittedInitialLoad = false

    public init(embed: LinkPreviewEmbed) {
        initialURL = embed.url
    }

    public mutating func decide(url: URL?, frame: Frame, isLink: Bool, hasTrustedButton: Bool) -> Decision {
        guard let url else { return .cancel }
        if isLink {
            guard hasTrustedButton, let scheme = url.scheme?.lowercased(), ["https", "http"].contains(scheme),
                  url.host()?.isEmpty == false, url.user() == nil, url.password() == nil
            else { return .cancel }
            return .openExternally(url)
        }
        switch frame {
        case .main:
            guard !admittedInitialLoad, let document = documentURL(url), document == documentURL(initialURL) else {
                return .cancel
            }
            admittedInitialLoad = true
            return .allow
        case .subframe:
            return allowsResponse(url: url, frame: .subframe) ? .allow : .cancel
        case .newWindow:
            return .cancel
        }
    }

    public func allowsResponse(url: URL?, frame: Frame) -> Bool {
        guard admittedInitialLoad, let url else { return false }
        switch frame {
        case .main: return documentURL(url) == documentURL(initialURL)
        case .subframe:
            return LinkPreviewEmbed.provider(for: url) != nil || ["about:blank", "about:srcdoc"].contains(url.absoluteString)
        case .newWindow: return false
        }
    }

    private func documentURL(_ url: URL) -> URL? {
        guard let provider = LinkPreviewEmbed.provider(for: url),
              var components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        components.scheme = "https"
        components.host = provider.rawValue
        components.port = nil
        if components.path.isEmpty { components.path = "/" }
        // HTTP responses can omit the fragment even when the page uses it to seek.
        components.fragment = nil
        return components.url
    }
}
