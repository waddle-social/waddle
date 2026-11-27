import Foundation
import XCTest
@testable import Waddle
import WaddleKit

@MainActor
final class MessageMediaSnapshotTests: XCTestCase {
    func testWithheldImage() {
        assertMessage(files: [Self.file("rink.jpg", mediaType: "image/jpeg")])
    }

    func testWithheldSeveralFiles() {
        // The withheld players and PDF add no rows to the consent prompt.
        assertMessage(files: [
            Self.file("rink.jpg", mediaType: "image/jpeg"),
            Self.file("highlights.mp4", mediaType: "video/mp4"),
            Self.file("commentary.mp3", mediaType: "audio/mpeg"),
            Self.file("lineup.pdf", mediaType: "application/pdf"),
        ])
    }

    func testWithheldBesideText() {
        assertMessage(body: "Here is a photo from the match.", files: [Self.file("rink.jpg", mediaType: "image/jpeg")])
    }

    func testWithheldVideoLink() throws {
        let page = URL(string: "https://vimeo.com/123")!
        let embed = try XCTUnwrap(LinkPreviewEmbed(url: URL(string: "https://player.vimeo.com/video/123")!))
        let preview = LinkPreview(
            url: page, title: "Highlights", summary: "Saturday's match",
            imageURL: nil, imageAlt: nil, originalURL: page.absoluteString, playback: .embed(embed)
        )
        assertMessage(files: [], previews: [preview])
    }

    private func assertMessage(
        body: String = "", files: [SharedFile], previews: [LinkPreview] = [], testName: String = #function, line: UInt = #line
    ) {
        let sample = SampleSession()
        let from = JID(parsing: "kai@away.example/mobile")!
        let conversation = ConversationID.direct(from.bare)
        let message = WireMessage(
            type: .chat, from: from, to: nil, identity: MessageIdentity(), body: body, sharedFiles: files, linkPreviews: previews
        )
        let item = TimelineItem(id: "sample-media", conversation: conversation, isMine: false, message: message, body: body)
        let entry = TimelineFeedEntry(
            item: item, daySeparator: nil, unreadDivider: nil, startsGroup: false, thread: nil, replyParent: nil
        )
        let actions = MessageActionModel(conversation: conversation, composer: ComposerModel(), thread: nil)
        assertHuddleSnapshot(
            of: sample.host(MessageRow(entry: entry) { _ in }.environment(actions)), testName: testName, line: line
        )
    }

    private static func file(_ name: String, mediaType: String) -> SharedFile {
        SharedFile(url: URL(string: "https://files.away.example/\(name)")!, name: name, mediaType: mediaType, disposition: .inline)
    }
}
