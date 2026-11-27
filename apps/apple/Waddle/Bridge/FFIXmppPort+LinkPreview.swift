import Foundation
import WaddleKit

extension FFIXmppPort {
    func lookupLinkPreview(_ request: LinkPreviewRequest) async throws -> LinkPreviewLookup {
        let result = try await mappingPortErrors {
            try await client.lookupLinkPreview(url: request.url.rawValue, scopeJid: request.scopeJid.description)
        }
        switch result.status {
        case .unsupported: return .unsupported
        case .blocked: return .blocked
        case .failed: return .failed
        case .ready:
            guard let payload = result.preview,
                  let url = FFIInbound.webURL(payload.normalizedUrl),
                  let expiry = FFIInbound.date(payload.expiresAt)
            else { return .failed }
            let image = FFIInbound.linkPreviewImage(payload.image, mediaOrigin: linkPreviewMediaOrigin)
            let preview = LinkPreview(
                url: url, title: payload.title, summary: payload.description,
                imageURL: image, imageAlt: image == nil ? nil : payload.image?.alt,
                originalURL: payload.originalUrl
            )
            let token = LinkPreviewToken(rawValue: payload.token)
            return .ready(LinkPreviewPayload(token: token, expiresAt: expiry, preview: preview))
        }
    }
}
