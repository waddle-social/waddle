import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct MessageLinkPreviewMediaTests {
    @Test func directFileMatchesTheOriginalOrCanonicalURLAndIsConsumedOnce() throws {
        let original = mediaURL("original")
        let canonical = mediaURL("canonical")
        for url in [canonical, original] {
            let download = SharedFile(url: url, mediaType: "video/mp4")
            let inline = SharedFile(url: url, mediaType: "application/x-mpegurl", disposition: .inline)
            let plain = LinkPreview(
                url: canonical, title: nil, summary: nil, imageURL: nil, imageAlt: nil, originalURL: original.absoluteString
            )
            let projected = MessageLinkPreviews(previews: [plain, plain], files: [download, inline])
            let video = try #require(LinkPreviewVideo(url: url, mediaType: "application/x-mpegurl"))
            #expect(projected.previews.map(\.playback) == [.video(video), nil])
            #expect(projected.remainingFiles == [download])
        }
    }

    @Test func directFileNeedsAMatchingLinkAndCannotReplaceAnExplicitPlayer() throws {
        let url = mediaURL("page")
        let otherURL = mediaURL("other.mp4")
        let embed = try #require(LinkPreviewEmbed(url: URL(string: "https://player.vimeo.com/video/123")!))
        let embedPreview = preview(url, playback: .embed(embed))
        let files = [
            SharedFile(url: url, mediaType: "video/mp4", disposition: .inline),
            SharedFile(url: otherURL, mediaType: "video/mp4", disposition: .inline),
        ]
        let projected = MessageLinkPreviews(previews: [embedPreview, preview(mediaURL("elsewhere"))], files: files)
        #expect(projected.previews.map(\.playback) == [.embed(embed), nil])
        #expect(projected.remainingFiles == files)
    }

    @Test func aVideoExtensionDoesNotOverrideTheFileMediaType() {
        let url = mediaURL("clip.mp4")
        let files = [SharedFile(url: url, mediaType: "application/pdf", disposition: .inline)]
        let projected = MessageLinkPreviews(previews: [preview(url)], files: files)
        #expect(projected.previews.first?.playback == nil)
        #expect(projected.remainingFiles == files)
    }

    @Test func withholdingStripsBothKindsOfPlayerAndItsPoster() throws {
        let videoURL = mediaURL("clip.mp4")
        let embed = try #require(LinkPreviewEmbed(url: URL(string: "https://player.vimeo.com/video/123")!))
        let poster = mediaURL("poster.jpg")
        var row = directRow()
        row.message.sharedFiles = [SharedFile(url: videoURL, mediaType: "video/mp4", disposition: .inline)]
        row.message.linkPreviews = [preview(videoURL), preview(mediaURL("watch"), image: poster, playback: .embed(embed))]
        let withheld = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        #expect(withheld.withheld?.items == [.linkPreviewImages, .linkPreviewVideos])
        #expect(withheld.linkPreviews.count == 2)
        #expect(withheld.linkPreviews.allSatisfy { $0.playback == nil && $0.imageURL == nil })
        #expect(withheld.visibleAttachments.isEmpty)

        let allowed = MessageMedia(item: row, decision: .allowed)
        let direct = try #require(LinkPreviewVideo(url: videoURL, mediaType: "video/mp4"))
        #expect(allowed.withheld == nil)
        #expect(allowed.linkPreviews.map(\.playback) == [.video(direct), .embed(embed)])
        #expect(allowed.linkPreviews.last?.imageURL == poster)
        #expect(allowed.visibleAttachments.isEmpty)
    }

    @Test func aPosterlessPlayerStillOffersConsentAndChangingEitherSourceNeedsConsentAgain() throws {
        let vimeo = try #require(LinkPreviewEmbed(url: URL(string: "https://player.vimeo.com/video/123")!))
        let youtube = try #require(LinkPreviewEmbed(url: URL(string: "https://www.youtube-nocookie.com/embed/clip")!))
        let video = try #require(LinkPreviewVideo(url: mediaURL("clip.mp4"), mediaType: "video/mp4"))
        let newVideo = try #require(LinkPreviewVideo(url: mediaURL("new.mp4"), mediaType: "video/mp4"))
        var row = directRow()
        let session = SessionCoordinator(account: me, port: FakePort())
        let url = mediaURL("watch")
        row.body = url.absoluteString
        row.message.body = row.body
        let destinations: [(LinkPreviewPlayback, LinkPreviewPlayback)] = [
            (.embed(vimeo), .embed(youtube)), (.video(video), .video(newVideo)),
        ]
        for (first, replacement) in destinations {
            row.message.linkPreviews = [preview(url, playback: first)]
            let withheld = MessageMedia(item: row, decision: session.remoteMediaDecision(for: row))
            #expect(withheld.withheld?.items == [.linkPreviewVideos])
            #expect(withheld.linkPreviews.first?.playback == nil)
            session.loadRemoteMedia(for: row)
            #expect(session.remoteMediaDecision(for: row) == .allowed)

            row.message.linkPreviews = [preview(url, playback: replacement)]
            #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("remote.test")))
        }
    }

    @Test func withholdingAlsoCoversInlineHLSFilesOutsideTheCard() {
        var row = directRow()
        row.message.sharedFiles = [
            SharedFile(url: mediaURL("stream"), name: "Live feed", mediaType: "application/x-mpegurl", disposition: .inline),
            SharedFile(url: mediaURL("playlist.m3u8"), name: "Playlist", disposition: .inline),
        ]
        let withheld = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        #expect(withheld.visibleAttachments.isEmpty)
        // Only images are named in the notice; other withheld files show the prompt alone.
        #expect(withheld.withheld?.items == [])
    }

    private func preview(_ url: URL, image: URL? = nil, playback: LinkPreviewPlayback? = nil) -> LinkPreview {
        LinkPreview(url: url, title: "Video", summary: nil, imageURL: image, imageAlt: nil, originalURL: nil, playback: playback)
    }
}
