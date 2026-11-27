import Foundation
import WaddleKit
import XCTest
@testable import Waddle

@MainActor
final class MessageLinkPreviewSnapshotTests: XCTestCase {
    func testNativeVideoPlayControl() throws {
        let url = URL(string: "https://media.example/highlights.m3u8")!
        let video = try XCTUnwrap(LinkPreviewVideo(url: url, mediaType: "application/vnd.apple.mpegurl"))
        assertHuddleSnapshot(of: MessageLinkPreviewCard(preview: preview(url, playback: .video(video))))
    }

    func testEmbeddedVideoPlayControl() throws {
        let url = URL(string: "https://vimeo.com/123")!
        let embed = try XCTUnwrap(LinkPreviewEmbed(
            url: URL(string: "https://player.vimeo.com/video/123")!, width: 640, height: 400
        ))
        assertHuddleSnapshot(of: MessageLinkPreviewCard(preview: preview(url, playback: .embed(embed))))
    }

    func testStaticLinkCardUsedForPlaybackFallback() {
        let url = URL(string: "https://media.example/highlights.m3u8")!
        assertHuddleSnapshot(of: MessageLinkPreviewCard(preview: preview(url, playback: nil)))
    }

    private func preview(_ url: URL, playback: LinkPreviewPlayback?) -> LinkPreview {
        LinkPreview(
            url: url, title: "Highlights", summary: "Saturday's match",
            imageURL: nil, imageAlt: nil, originalURL: url.absoluteString, playback: playback
        )
    }
}
