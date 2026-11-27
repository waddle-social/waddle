import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct MessageMediaProjectionTests {
    @Test(arguments: [
        ("voice.ogg", "opaque", nil),
        (nil, "voice.OGA", "audio/mpeg"),
        ("clip.OGV", "opaque", "video/mp4"),
        (nil, "clip.webm", nil),
        (nil, "opaque", "Audio/Ogg; codecs=opus"),
        (nil, "opaque", "video/ogg"),
        (nil, "opaque", "application/ogg"),
        (nil, "opaque", "audio/webm"),
        (nil, "opaque", "video/webm"),
    ] as [(String?, String, String?)])
    func unsupportedAppleContainersUseFileCards(name: String?, path: String, type: String?) throws {
        var row = directRow()
        row.message.sharedFiles = [SharedFile(url: mediaURL(path), name: name, mediaType: type, disposition: .inline)]
        let attachment = try #require(MessageMedia(item: row, decision: .allowed).attachments.first)
        #expect(attachment.presentation == .file)
        #expect(!attachment.presentation.hasInlineControls)
    }

    @Test func projectsTheVisibleAttachmentPresentation() {
        var row = directRow()
        let encrypted = EncryptedFileSource(
            cipher: .aes256GCM, keyBase64: "", ivBase64: "", digests: FileDigests(), sources: [mediaURL("payload.enc")]
        )
        row.message.sharedFiles = [
            SharedFile(url: mediaURL("opaque"), name: "voice.mp3", disposition: .inline),
            SharedFile(url: mediaURL("clip.mov"), disposition: .inline),
            SharedFile(url: mediaURL("opaque"), name: "report.pdf", disposition: .inline),
            SharedFile(url: mediaURL("photo.png"), disposition: .inline),
            SharedFile(url: mediaURL("download.mp4")),
            SharedFile(url: URL(string: "http://remote.test/clip.mp4")!, disposition: .inline),
            SharedFile(url: mediaURL("encrypted.mp3"), disposition: .inline, encrypted: encrypted),
        ]
        let shown = MessageMedia(item: row, decision: .allowed).attachments.map(\.presentation)
        #expect(shown == [.audio, .video, .pdf, .image, .file, .file, .encrypted(encrypted)])
        #expect(shown.filter(\.hasInlineControls) == [.audio, .video, .pdf])
    }

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
        ])
        #expect(MessageMedia(item: row, decision: .allowed).visibleAttachments == row.message.sharedFiles)
    }

    @Test func withholdingDoesNotExposeImageURLsToRenderers() throws {
        var row = directRow()
        row.body = mediaURL("clip.gif").absoluteString
        let preview = LinkPreview(url: mediaURL("page"), title: "Title", summary: "Summary", imageURL: mediaURL("poster.png"), imageAlt: "Poster", originalURL: nil)
        row.message.linkPreviews = [preview]

        let presented = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        let textPreview = try #require(presented.linkPreviews.first)
        #expect(presented.inlineBody == .withheld)
        #expect(presented.visibleAttachments.isEmpty)
        #expect(MediaGallery.images(in: row, media: presented).isEmpty)
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
            LinkPreview(url: mediaURL("page"), title: nil, summary: nil, imageURL: mediaURL("poster.png"), imageAlt: nil, originalURL: nil),
        ]

        let withheld = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        #expect(withheld.inlineBody == .text) // Attachments take precedence over the image URL in the body.
        #expect(withheld.visibleAttachments.isEmpty)
        #expect(MediaGallery.images(in: row, media: withheld).isEmpty)
        #expect(withheld.stickerPackLocation == nil)
        #expect(withheld.linkPreviews.compactMap(\.imageURL).isEmpty)
        #expect(withheld.withheld?.items == [.attachment(mediaURL("sticker.gif").absoluteString), .linkPreviewImages])

        let allowed = MessageMedia(item: row, decision: .allowed)
        #expect(allowed.stickerPackLocation != nil)
        #expect(allowed.visibleAttachments == row.message.sharedFiles)
        #expect(allowed.linkPreviews == row.message.linkPreviews)
    }

    @Test func withheldPlayersAndPDFsExposeOnlyTheConsentPrompt() {
        var row = directRow()
        row.message.sharedFiles = ["Recording.mp3", "Clip.mov", "Agenda.pdf"].map {
            SharedFile(url: mediaURL($0), name: $0, disposition: .inline)
        }

        let withheld = MessageMedia(item: row, decision: .withheld(host: Host("remote.test")))
        #expect(withheld.attachments.isEmpty)
        #expect(withheld.withheld?.items == [])
        #expect(MessageMedia(item: row, decision: .allowed).visibleAttachments == row.message.sharedFiles)
    }
}
