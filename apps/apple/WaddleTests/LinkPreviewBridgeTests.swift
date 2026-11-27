import Foundation
import WaddleKit
import XCTest
@testable import Waddle

final class LinkPreviewBridgeTests: XCTestCase {
    func testVideoAndEmbedFieldsCrossTheBridge() throws {
        let stream = "https://media.example/stream.m3u8"
        let video = WaddleLinkPreviewVideo(url: stream, mediaType: "application/vnd.apple.mpegurl")
        let native = try XCTUnwrap(FFIInbound.linkPreview(preview(video: video), mediaOrigin: nil))
        guard case let .video(source) = native.playback else { return XCTFail("Expected a native video") }
        XCTAssertEqual(source.url.absoluteString, stream)

        let playerURL = "https://player.vimeo.com/video/123"
        let player = WaddleLinkPreviewPlayer(url: playerURL, width: 640, height: 400)
        let embedded = try XCTUnwrap(FFIInbound.linkPreview(preview(player: player), mediaOrigin: nil))
        guard case let .embed(embed) = embedded.playback else { return XCTFail("Expected a Vimeo player") }
        XCTAssertEqual(embed.url.absoluteString, playerURL)
        XCTAssertEqual(embed.aspectRatio, 1.6)
    }

    func testUntrustedPosterAndPlayerProduceAPlainLink() throws {
        let image = WaddleLinkPreviewImage(
            url: "https://media.example/poster.png", mediaType: "image/png", width: 640, height: 400, alt: "Poster"
        )
        let player = WaddleLinkPreviewPlayer(url: "https://www.youtube.com/embed/clip", width: nil, height: nil)
        let card = try XCTUnwrap(FFIInbound.linkPreview(
            preview(image: image, player: player), mediaOrigin: URL(string: "https://waddle.example")
        ))
        XCTAssertNil(card.playback)
        XCTAssertNil(card.imageURL)
        XCTAssertNil(card.imageAlt)
    }

    private func preview(
        image: WaddleLinkPreviewImage? = nil, video: WaddleLinkPreviewVideo? = nil, player: WaddleLinkPreviewPlayer? = nil
    ) -> WaddleLinkPreview {
        WaddleLinkPreview(
            originalUrl: "https://media.example/watch", normalizedUrl: nil, title: "Highlights", description: nil,
            image: image, video: video, playerEmbed: player, remoteMediaUnavailable: false
        )
    }
}
