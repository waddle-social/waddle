import Foundation
import Testing
@testable import WaddleKit

struct LinkPreviewPlaybackTests {
    private let videoURL = URL(string: "https://media.example/stream.m3u8")!
    private let youtubeURL = URL(string: "https://www.youtube-nocookie.com/embed/clip")!

    @Test func aPlainLinkCannotStartAPlayer() {
        var card = LinkPreviewCardState(playback: nil)
        card.play(muted: true)
        card.fail()
        #expect(card == .link)
    }

    @Test func aNativeVideoTakesPrecedenceOverAnEmbed() throws {
        let video = try #require(LinkPreviewVideo(url: videoURL, mediaType: "application/vnd.apple.mpegurl"))
        #expect(video.isHLS)
        let embed = try #require(LinkPreviewEmbed(url: youtubeURL))
        #expect(LinkPreviewPlayback(video: video, embed: embed) == .video(video))
        #expect(LinkPreviewPlayback(video: nil, embed: embed) == .embed(embed))
        #expect(LinkPreviewPlayback(video: nil, embed: nil) == nil)
    }

    @Test func pressingPlayStartsMutedInACallAndFatalFailureFallsBack() throws {
        let video = try #require(LinkPreviewVideo(url: videoURL, mediaType: "application/vnd.apple.mpegurl"))
        let playback = LinkPreviewPlayback.video(video)
        var card = LinkPreviewCardState(playback: playback)
        #expect(card == .ready(playback))

        card.play(muted: true)
        #expect(card == .video(video, muted: true))
        card.stop(playback: playback)
        #expect(card == .ready(playback))

        card.play(muted: false)
        #expect(card == .video(video, muted: false))
        card.fail()
        card.stop(playback: playback)
        card.play(muted: true)
        #expect(card == .fallback)
    }

    @Test(arguments: [
        "video/mp4", " VIDEO/QUICKTIME; codecs=avc1", "video/webm", "video/ogg",
        "application/vnd.apple.mpegurl", "application/x-mpegurl", "application/mpegurl",
        "audio/mpegurl", "audio/x-mpegurl",
    ])
    func nativeVideoAcceptsTheSupportedMIMETypes(_ mimeType: String) {
        #expect(LinkPreviewVideo(url: videoURL, mediaType: mimeType)?.url == videoURL)
    }

    @Test func nativeVideoRejectsOtherMIMETypesAndUnsafeURLs() {
        #expect(LinkPreviewVideo(url: videoURL, mediaType: "text/html") == nil)
        for rawURL in ["http://media.example/clip", "https://user@media.example/clip", "https:///clip", "clip"] {
            #expect(URL(string: rawURL).flatMap { LinkPreviewVideo(url: $0, mediaType: "video/mp4") } == nil)
        }
    }

    @Test(arguments: [
        "https://www.youtube-nocookie.com:443/embed/clip",
        "HTTPS://PLAYER.VIMEO.COM/video/123",
    ])
    func acceptsOnlyTheFinalPlayerOriginsOnTheHTTPSPort(_ rawURL: String) throws {
        let embed = try #require(LinkPreviewEmbed(url: URL(string: rawURL)!))
        #expect(embed.aspectRatio == 16.0 / 9.0)
    }

    @Test(arguments: [
        "http://player.vimeo.com/video/123", "https://www.youtube.com/embed/clip",
        "https://www.youtube-nocookie.com.evil.example/embed/clip", "https://vimeo.com/video/123",
        "https://user:pass@player.vimeo.com/video/123", "https://evil.example@player.vimeo.com/video/123",
        "https://player.vimeo.com:444/video/123", "https://player.vimeo.com./video/123",
        "https://player.vimeo.com:9999999999999999999999/video/123", "/video/123",
    ])
    func rejectsOtherOriginsAndCredentials(_ rawURL: String) {
        #expect(URL(string: rawURL).flatMap { LinkPreviewEmbed(url: $0) } == nil)
    }

    @Test func pressingPlayOwnsAutoplayAndProviderMuteParameters() throws {
        let cases: [(LinkPreviewEmbed.Provider, String, String, String)] = [
            (.youtube, "www.youtube-nocookie.com/embed/clip", "start=12", "mute"),
            (.vimeo, "player.vimeo.com/video/123", "h=abc%2B123", "muted"),
        ]
        for (provider, source, preserved, muteKey) in cases {
            let url = try #require(URL(string: "https://\(source)?\(preserved)&autoplay=0&mute=0&muted=0&%61utoplay=2"))
            let embed = try #require(LinkPreviewEmbed(url: url, width: 640, height: 400))
            for inCall in [false, true] {
                var card = LinkPreviewCardState(playback: .embed(embed))
                #expect(card == .ready(.embed(embed)))
                card.play(muted: inCall)
                guard case let .embed(player) = card else {
                    Issue.record("A press should start the embed")
                    continue
                }
                let items = URLComponents(url: player.url, resolvingAgainstBaseURL: false)?.queryItems ?? []
                #expect(player.provider == provider)
                #expect(player.aspectRatio == 1.6)
                #expect(player.url.host() == url.host())
                #expect(player.url.absoluteString.contains(preserved))
                #expect(items.filter { $0.name == "autoplay" }.map(\.value) == ["1"])
                #expect(items.filter { ["mute", "muted"].contains($0.name) }
                    == (inCall ? [URLQueryItem(name: muteKey, value: "1")] : []))
                #expect(items.first { $0.name == "playsinline" }?.value == (provider == .youtube ? "1" : nil))
                card.fail()
                #expect(card == .fallback)
            }
            #expect(embed.url == url)
        }
    }

    @Test func playerNavigationAllowsOneInitialDocumentAndRejectsOtherTopFrameLoads() throws {
        var navigation = LinkPreviewEmbedNavigation(embed: try #require(LinkPreviewEmbed(url: youtubeURL)))
        let sameOrigin = URL(string: "https://www.youtube-nocookie.com/embed/another")!
        let external = URL(string: "https://example.com/watch")!
        let otherPlayer = URL(string: "https://player.vimeo.com/video/123")!
        let script = URL(string: "javascript:alert(1)")!

        func decide(
            _ url: URL,
            frame: LinkPreviewEmbedNavigation.Frame,
            isLink: Bool = false,
            hasTrustedButton: Bool = false
        ) -> LinkPreviewEmbedNavigation.Decision {
            navigation.decide(url: url, frame: frame, isLink: isLink, hasTrustedButton: hasTrustedButton)
        }

        #expect(!navigation.allowsResponse(url: youtubeURL, frame: .main))
        #expect(decide(sameOrigin, frame: .main) == .cancel)
        #expect(decide(youtubeURL, frame: .main) == .allow)
        #expect(decide(youtubeURL, frame: .main) == .cancel)
        #expect(decide(sameOrigin, frame: .main) == .cancel)
        #expect(decide(otherPlayer, frame: .main) == .cancel)
        #expect(decide(external, frame: .main) == .cancel)
        #expect(decide(external, frame: .subframe) == .cancel)
        #expect(decide(youtubeURL, frame: .subframe) == .allow)
        #expect(decide(external, frame: .main, isLink: true) == .cancel)
        #expect(decide(youtubeURL, frame: .subframe, isLink: true) == .cancel)
        #expect(decide(external, frame: .newWindow, isLink: true) == .cancel)
        #expect(decide(external, frame: .main, isLink: true, hasTrustedButton: true) == .openExternally(external))
        #expect(decide(external, frame: .newWindow, isLink: true, hasTrustedButton: true) == .openExternally(external))
        #expect(decide(youtubeURL, frame: .newWindow) == .cancel)
        #expect(decide(script, frame: .main, isLink: true, hasTrustedButton: true) == .cancel)
        #expect(!navigation.allowsResponse(url: external, frame: .main))
        #expect(!navigation.allowsResponse(url: sameOrigin, frame: .main))
        #expect(navigation.allowsResponse(url: youtubeURL, frame: .main))
    }

    @Test func theInitialResponseMayOmitThePlayerTimeFragment() throws {
        let player = URL(string: "HTTPS://PLAYER.VIMEO.COM:443/video/123?autoplay=1#t=5s")!
        let response = URL(string: "https://player.vimeo.com/video/123?autoplay=1")!
        var navigation = LinkPreviewEmbedNavigation(embed: try #require(LinkPreviewEmbed(url: player)))
        let action = navigation.decide(url: response, frame: .main, isLink: false, hasTrustedButton: false)
        #expect(action == .allow)
        #expect(navigation.allowsResponse(url: response, frame: .main))
    }
}
