import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct MessageMediaProjectionTests {
    @Test func withheldFilesLeaveExplicitDownloadCardsAvailable() {
        var row = directRow()
        let encrypted = EncryptedFileSource(
            cipher: .aes256GCM, keyBase64: "", ivBase64: "", digests: FileDigests(), sources: [mediaURL("payload.enc")]
        )
        let downloads = [
            SharedFile(url: mediaURL("archive.zip")),
            SharedFile(url: mediaURL("download.mp4"), mediaType: "video/mp4"),
            SharedFile(url: mediaURL("download.pdf"), mediaType: "application/pdf", encrypted: encrypted),
        ]
        row.message.sharedFiles = downloads + [
            SharedFile(url: mediaURL("sticker.gif"), mediaType: "IMAGE/GIF"),
            SharedFile(url: mediaURL("encrypted-image"), name: "photo.png", mediaType: "image/png", encrypted: encrypted),
            SharedFile(url: mediaURL("movie.webm"), disposition: .inline),
            SharedFile(url: mediaURL("voice.ogg"), disposition: .inline, encrypted: encrypted),
            SharedFile(url: mediaURL("book.pdf"), disposition: .inline),
        ]
        row.message.isSticker = true

        let presented = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        #expect(presented.visibleAttachments == downloads)
        #expect(presented.withheld?.items == [
            .attachment(mediaURL("sticker.gif").absoluteString), .attachment("photo.png"),
            .attachment(mediaURL("movie.webm").absoluteString), .attachment(mediaURL("voice.ogg").absoluteString),
            .attachment(mediaURL("book.pdf").absoluteString),
        ])
        #expect(MessageMedia(item: row, decision: .allowed).visibleAttachments == row.message.sharedFiles)
    }

    @Test func withholdingDoesNotExposeImageURLsToRenderers() throws {
        var row = directRow()
        row.body = mediaURL("clip.gif").absoluteString
        let preview = LinkPreview(url: mediaURL("page"), title: "Title", summary: "Summary", imageURL: mediaURL("poster.png"), imageAlt: "Poster")
        row.message.linkPreviews = [preview]

        let presented = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        let textPreview = try #require(presented.linkPreviews.first)
        #expect(presented.inlineBody == .withheld)
        #expect(presented.visibleAttachments.isEmpty)
        #expect(presented.previewableImageURLs.isEmpty)
        #expect(presented.stickerPackLocation == nil)
        #expect(textPreview.title == preview.title)
        #expect(textPreview.summary == preview.summary)
        #expect(textPreview.imageURL == nil)
        #expect(presented.withheld?.items == [.inlineImage(row.body), .linkPreviewImages])

        let allowed = MessageMedia(item: row, decision: .allowed)
        #expect(allowed.inlineBody == .image(mediaURL("clip.gif")))
        #expect(allowed.linkPreviews == [preview])
        #expect(allowed.withheld == nil)
    }
    @Test func aWithheldStickerAndAttachmentsExposeNoFetchTargets() {
        var row = directRow()
        row.message.isSticker = true
        row.message.stickerPack = StickerReference(packID: "cats")
        row.message.sharedFiles = [
            SharedFile(url: mediaURL("sticker.gif"), mediaType: "image/gif"),
            SharedFile(url: mediaURL("sound.mp3"), disposition: .inline),
        ]
        row.message.linkPreviews = [
            LinkPreview(url: mediaURL("page"), title: nil, summary: nil, imageURL: mediaURL("poster.png"), imageAlt: nil),
        ]

        let withheld = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        #expect(withheld.inlineBody == .text) // Attachments take precedence over the image URL in the body.
        #expect(withheld.visibleAttachments.isEmpty)
        #expect(withheld.previewableImageURLs.isEmpty)
        #expect(withheld.stickerPackLocation == nil)
        #expect(withheld.linkPreviews.compactMap(\.imageURL).isEmpty)
        #expect(withheld.withheld?.items.count == 3)

        let allowed = MessageMedia(item: row, decision: .allowed)
        #expect(allowed.stickerPackLocation != nil)
        #expect(allowed.visibleAttachments == row.message.sharedFiles)
        #expect(allowed.linkPreviews == row.message.linkPreviews)
    }

}
