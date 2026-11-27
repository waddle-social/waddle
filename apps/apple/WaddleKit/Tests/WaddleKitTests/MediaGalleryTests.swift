import Foundation
import Testing
@testable import WaddleKit

struct MediaGalleryTests {
    @Test func ordersAllImagesByRowAndAttachmentOrder() throws {
        let first = file("first.jpg", type: "IMAGE/JPEG")
        let second = file("second.gif", type: "image/gif", description: "The team's mascot")
        let post = row(files: [first, file("notes.pdf", type: "application/pdf"), second])
        let inline = row(body: mediaURL("last.png").absoluteString)

        let images = eligibleImages(in: post)
        let gallery = try #require(open([post, inline], selected: images[1].id))
        #expect(gallery.images.map(\.file.url) == [first.url, second.url, mediaURL("last.png")])
        #expect(gallery.current.file == second)
        #expect(gallery.current.name == "second.gif")
        #expect(gallery.current.accessibilityName == "The team's mascot")
        #expect(gallery.position == 2)
        #expect(gallery.count == 3)
        #expect(gallery.neighbor(.previous) == images[0].id)
        #expect(gallery.neighbor(.next) == eligibleImages(in: inline)[0].id)

        let reversed = try #require(open([inline, post], selected: images[1].id))
        #expect(reversed.position == 3)
        #expect(reversed.neighbor(.next) == nil)
        let start = try #require(open([inline, post], selected: eligibleImages(in: inline)[0].id))
        #expect(start.current.accessibilityName == "last.png")
        #expect(start.position == 1)
        #expect(start.neighbor(.previous) == nil)
    }

    @Test func withheldRowsAppearOnlyAfterConsentAndAnInvalidSelectionCloses() throws {
        let local = row(body: mediaURL("local.png").absoluteString)
        let foreign = row(files: [file("foreign.jpg", type: "image/jpeg")])
        let foreignInline = row(body: mediaURL("foreign.gif").absoluteString)
        let localID = try #require(eligibleImages(in: local).first?.id)
        let foreignID = try #require(eligibleImages(in: foreign).first?.id)
        let items = [foreign, local, foreignInline]
        let withheld: (TimelineItem) -> RemoteMediaDecision = {
            $0.presentationID == local.presentationID ? .allowed : .withheld(host: nil)
        }

        let gated = try #require(MediaGallery(items: items, selected: localID, decision: withheld))
        #expect(gated.images.map(\.id) == [localID])
        #expect(gated.neighbor(.previous) == nil)
        #expect(gated.neighbor(.next) == nil)
        #expect(MediaGallery(items: items, selected: foreignID, decision: withheld) == nil)

        let allowed = try #require(open(items, selected: localID))
        #expect(allowed.position == 2)
        #expect(allowed.count == 3)
        #expect(allowed.neighbor(.previous) == foreignID)
        #expect(open([local, foreignInline], selected: foreignID) == nil)
        #expect(open([], selected: localID) == nil)

        var corrected = foreign
        corrected.message.sharedFiles = [file("different.jpg", type: "image/jpeg")]
        #expect(open([corrected, local], selected: foreignID) == nil)
    }

    @Test func distinguishesIdenticalFilesAndDoesNotRetargetSelectionOnEdits() throws {
        let repeated = file("source.jpg", type: "image/jpeg")
        let sameURL = SharedFile(url: repeated.url, name: "alternate.jpg", mediaType: "image/jpeg")
        let original = row(files: [sameURL, repeated, repeated])
        let anotherRow = row(files: [repeated])
        let images = eligibleImages(in: original)
        let selected = try #require(images.last?.id)
        let gallery = try #require(open([original, anotherRow], selected: selected))
        #expect(Set(gallery.images.map(\.id)).count == 4)
        #expect(gallery.position == 3)

        var edited = original
        edited.message.sharedFiles.removeFirst()
        let updated = try #require(open([edited, anotherRow], selected: selected))
        #expect(updated.position == 2)
        #expect(open([edited, anotherRow], selected: images[0].id) == nil)
    }

    @Test func skipsEncryptedImagesStickersTombstonesAndLinkPreviews() throws {
        let encrypted = EncryptedFileSource(
            cipher: .aes256GCM, keyBase64: "", ivBase64: "", digests: FileDigests(), sources: [mediaURL("payload.enc")]
        )
        let encryptedRow = row(files: [SharedFile(url: mediaURL("locked.png"), mediaType: "image/png", encrypted: encrypted)])
        var sticker = row(files: [file("sticker.png", type: "image/png")])
        sticker.message.isSticker = true
        var removed = row(files: [file("removed.png", type: "image/png")])
        removed.tombstone = .retracted
        var preview = row(body: "See this link")
        preview.message.linkPreviews = [
            LinkPreview(
                url: mediaURL("page"), title: nil, summary: nil, imageURL: mediaURL("poster.png"), imageAlt: nil, originalURL: nil
            )
        ]
        let inline = row(body: mediaURL("inline.gif").absoluteString)
        let id = try #require(eligibleImages(in: inline).first?.id)
        let gallery = try #require(open([encryptedRow, sticker, removed, inline, preview], selected: id))
        #expect(gallery.images.map(\.id) == [id])
    }

    @Test func callCardsDoNotContributePictures() {
        let conversation = foreignRoom
        var message = WireMessage(
            type: .groupchat, from: JID(bare: conversation.jid, resource: nil), to: nil,
            identity: MessageIdentity(), sharedFiles: [file("banner.png", type: "image/png")]
        )
        message.callThread = CallThreadAnchor(kind: .room, hasVideo: false, started: nil)
        let item = TimelineItem(id: "call", conversation: conversation, isMine: false, message: message, body: "")
        #expect(item.callAnchor != nil)
        #expect(eligibleImages(in: item).isEmpty)
    }

    @Test func anInlineImageCanOpenWhenItsMessageHasAStickerMarkerButNoAttachment() {
        let original = mediaURL("sticker.gif?signature=123")
        var inline = row(body: original.absoluteString)
        inline.message.isSticker = true
        #expect(eligibleImages(in: inline).map(\.file.url) == [original])
    }

    private func row(body: String = "", files: [SharedFile] = []) -> TimelineItem {
        var item = directRow()
        item.body = body
        item.message.sharedFiles = files
        return item
    }

    private func file(_ name: String, type: String, description: String? = nil) -> SharedFile {
        SharedFile(url: mediaURL(name), name: name, mediaType: type, description: description)
    }

    private func eligibleImages(in item: TimelineItem) -> [MediaGallery.Image] {
        MediaGallery.images(in: item, media: MessageMedia(item: item, decision: .allowed))
    }

    private func open(_ items: [TimelineItem], selected: MediaGallery.Image.ID) -> MediaGallery? {
        MediaGallery(items: items, selected: selected) { _ in .allowed }
    }
}
