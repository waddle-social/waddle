import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct RemoteMediaConsentTests {
    @Test func consentIsForOneMessageAndClearedOnSignOut() async {
        let session = SessionCoordinator(account: me, port: FakePort())
        let first = directRow()
        let sameWireID = directRow()
        let anotherConversation = directRow(from: bare("carol@elsewhere.test"))

        session.loadRemoteMedia(for: first)
        #expect(session.remoteMediaDecision(for: sameWireID) == .withheld(host: Host("remote.test")))
        #expect(session.remoteMediaDecision(for: anotherConversation) == .withheld(host: Host("elsewhere.test")))
        #expect(session.remoteMediaDecision(for: first) == .allowed)

        await session.signOut()
        #expect(session.remoteMediaDecision(for: first) == .withheld(host: Host("remote.test")))
    }

    @Test func consentSurvivesRebuildingAThreadFromItsArchive() throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        var message = roomRow(in: foreignRoom).message
        message.source = .archive(mamID: "archive-1")
        let thread = ThreadKey(room: foreignRoom.jid, threadID: "message")
        let page = ArchivePage(messages: [message], first: "archive-1", isComplete: true)

        let first = try #require(ThreadHistory.build(from: page, thread: thread, account: me).root)
        session.loadRemoteMedia(for: first)
        let reopened = try #require(ThreadHistory.build(from: page, thread: thread, account: me).root)
        #expect(reopened.presentationID != first.presentationID)
        #expect(session.remoteMediaDecision(for: reopened) == .allowed)

        let anotherRoom = ConversationID.room(bare("other@chat.remote.test"))
        var other = roomRow(in: anotherRoom)
        other.message.source = .archive(mamID: "archive-1")
        #expect(session.remoteMediaDecision(for: other) == .withheld(host: Host("chat.remote.test")))

        var live = roomRow(in: foreignRoom)
        live.message.identity = first.identity
        #expect(session.remoteMediaDecision(for: live) == .withheld(host: Host("chat.remote.test")))
    }

    @Test func sameArchiveIDWithDifferentMediaNeedsConsentAgain() {
        let session = SessionCoordinator(account: me, port: FakePort())
        var first = roomRow(in: foreignRoom)
        first.message.source = .archive(mamID: "archive-1")
        session.loadRemoteMedia(for: first)

        var replacement = roomRow(in: foreignRoom)
        replacement.message.source = first.message.source
        replacement.body = "https://images.example/replacement.gif"
        #expect(session.remoteMediaDecision(for: replacement) == .withheld(host: Host("chat.remote.test")))
    }

    @Test func changingOnlyTextKeepsConsentButAddingOrSwappingAnAttachmentNeedsConsentAgain() throws {
        let session = SessionCoordinator(account: me, port: FakePort())
        var original = roomRow(in: foreignRoom).message
        original.body = "Original caption"
        let firstFile = SharedFile(url: mediaURL("first.png"), mediaType: "image/png")
        let secondFile = SharedFile(url: mediaURL("second.png"), mediaType: "image/png")
        original.sharedFiles = [firstFile]
        session.timelines.ingest(original)
        let timeline = session.timelines.timeline(for: foreignRoom)
        session.loadRemoteMedia(for: try #require(timeline.items.first))

        var correction = original
        correction.replacesID = "message"
        correction.identity = MessageIdentity(messageID: "edit")
        correction.body = "New caption"
        correction.sharedFiles = []
        session.timelines.ingest(correction)
        let captionEdit = try #require(timeline.items.first)
        #expect(captionEdit.body == correction.body)
        #expect(session.remoteMediaDecision(for: captionEdit) == .allowed)

        for files in [[firstFile, secondFile], [secondFile]] {
            correction.sharedFiles = files
            session.timelines.ingest(correction)
            let attachmentEdit = try #require(timeline.items.first)
            #expect(attachmentEdit.message.sharedFiles == files)
            #expect(session.remoteMediaDecision(for: attachmentEdit) == .withheld(host: Host("chat.remote.test")))
        }
    }

    @Test func changingAnEncryptedSourceOrPreviewImageNeedsConsentAgain() {
        let session = SessionCoordinator(account: me, port: FakePort())
        let fileURL = mediaURL("cover.png")
        func encryptedFile(_ source: String) -> SharedFile {
            let encrypted = EncryptedFileSource(
                cipher: .aes256GCM, keyBase64: "", ivBase64: "", digests: FileDigests(), sources: [mediaURL(source)]
            )
            return SharedFile(url: fileURL, mediaType: "image/png", encrypted: encrypted)
        }
        func preview(_ picture: String) -> LinkPreview {
            LinkPreview(url: mediaURL("page"), title: nil, summary: nil, imageURL: mediaURL(picture), imageAlt: nil, originalURL: nil)
        }
        var row = directRow()
        row.message.sharedFiles = [encryptedFile("one.enc")]
        row.message.linkPreviews = [preview("poster.png")]
        session.loadRemoteMedia(for: row)

        var withOtherSource = row
        withOtherSource.message.sharedFiles = [encryptedFile("two.enc")]
        #expect(session.remoteMediaDecision(for: withOtherSource) == .withheld(host: Host("remote.test")))

        var withOtherImage = row
        withOtherImage.message.linkPreviews = [preview("other-poster.png")]
        #expect(session.remoteMediaDecision(for: withOtherImage) == .withheld(host: Host("remote.test")))
    }
    @Test func attachmentOrderAndACardBecomingInlineChangeConsent() {
        let session = SessionCoordinator(account: me, port: FakePort())
        var row = directRow()
        let image = SharedFile(url: mediaURL("first.png"), mediaType: "image/png")
        let download = SharedFile(url: mediaURL("report.pdf"), mediaType: "application/pdf")
        row.message.sharedFiles = [image, download]
        session.loadRemoteMedia(for: row)

        row.message.sharedFiles = [download, image]
        #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("remote.test")))

        let inline = SharedFile(url: download.url, mediaType: download.mediaType, disposition: .inline)
        row.message.sharedFiles = [image, inline]
        #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("remote.test")))
    }

    @Test(arguments: ["mp3", "mov", "pdf"])
    func changingAnInlineMediaURLRequiresConsentAgain(suffix: String) {
        let session = SessionCoordinator(account: me, port: FakePort())
        var row = directRow()
        row.message.sharedFiles = [SharedFile(url: mediaURL("first.\(suffix)"), disposition: .inline)]
        session.loadRemoteMedia(for: row)
        #expect(session.remoteMediaDecision(for: row) == .allowed)

        row.message.sharedFiles = [SharedFile(url: mediaURL("second.\(suffix)"), disposition: .inline)]
        #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("remote.test")))
    }
}
