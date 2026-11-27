import Foundation
import Testing
import WaddleKit

struct LinkPreviewMediaTests {
    private let origin = URL(string: "https://waddle.example")!
    private let slot = "11111111-1111-4111-8111-111111111111"
    private let filename = "link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png"

    @Test func sessionMediaOriginPrefersTheExplicitOrigin() {
        #expect(LinkPreviewMedia.origin(explicit: " https://cdn.example ", webSocket: "wss://xmpp.example/ws")
            == URL(string: "https://cdn.example"))
        #expect(LinkPreviewMedia.origin(explicit: " ", webSocket: "wss://xmpp.example/ws?trace=1")
            == URL(string: "https://xmpp.example"))
        #expect(LinkPreviewMedia.origin(explicit: "invalid", webSocket: "wss://xmpp.example/ws") == nil)
        #expect(LinkPreviewMedia.origin(explicit: nil, webSocket: "https://xmpp.example/ws") == nil)
    }

    @Test func trustedLoopbackHTTPAndExplicitDefaultPortsAreAccepted() {
        let loopbackOrigin = LinkPreviewMedia.origin(explicit: nil, webSocket: "ws://localhost:4321/xmpp")
        #expect(loopbackOrigin == URL(string: "http://localhost:4321"))
        #expect(trustedImage(at: "http://localhost:4321", origin: loopbackOrigin) != nil)
        #expect(trustedImage(at: "http://localhost:80", origin: URL(string: "http://localhost")) != nil)
        #expect(trustedImage(at: "https://WADDLE.example:443", origin: origin) != nil)
        #expect(trustedImage(at: "http://127.0.0.1", origin: URL(string: "http://127.0.0.1")) != nil)
        #expect(trustedImage(at: "http://[::1]", origin: URL(string: "http://[::1]")) != nil)
    }

    @Test func onlyTheSameTrustedOriginCanServeAnImage() {
        #expect(trustedImage(at: "https://waddle.example", origin: origin) != nil)
        #expect(trustedImage(at: "https://waddle.example", origin: nil) == nil)
        #expect(trustedImage(at: "https://foreign.example", origin: origin) == nil)
        #expect(trustedImage(at: "https://waddle.example:4321", origin: origin) == nil)
        #expect(trustedImage(at: "http://waddle.example", origin: URL(string: "http://waddle.example")) == nil)
        #expect(trustedImage(at: "https://localhost", origin: URL(string: "http://localhost")) == nil)
        #expect(trustedImage(at: "file://waddle.example", origin: origin) == nil)
    }

    @Test func imageRequiresACachedPreviewUploadPathAndSafeMediaType() {
        let path = "/api/files/\(slot)/\(filename)"
        let image = origin.absoluteString + path
        #expect(LinkPreviewMedia.trustedImageURL(image + "?key=value#fragment", mediaType: "image/png", origin: origin) != nil)
        #expect(LinkPreviewMedia.trustedImageURL(image, mediaType: "image/svg+xml", origin: origin) == nil)
        #expect(LinkPreviewMedia.trustedImageURL(image, mediaType: nil, origin: origin) == nil)
        let invalidPaths = [
            "/other/files/\(slot)/\(filename)",
            "/api/files/11111111-1111-6111-8111-111111111111/\(filename)",
            "/api/files/11111111-1111-4111-7111-111111111111/\(filename)",
            "/api/files/\(slot)/thumbnail.png",
            "/api/files/\(slot)/link-preview-123.png",
            "/api/files/\(slot)/\(filename).svg",
            "/api/files/\(slot)/\(filename)/child",
            "/api/%66iles/\(slot)/\(filename)",
        ]
        for path in invalidPaths {
            #expect(LinkPreviewMedia.trustedImageURL(origin.absoluteString + path, mediaType: "image/png", origin: origin) == nil)
        }
    }

    private func trustedImage(at base: String, origin: URL?) -> URL? {
        LinkPreviewMedia.trustedImageURL("\(base)/api/files/\(slot)/\(filename)", mediaType: "image/png", origin: origin)
    }
}
