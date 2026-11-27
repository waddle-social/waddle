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
        assertMessage(files: [
            Self.file("rink.jpg", mediaType: "image/jpeg"),
            Self.file("highlights.mp4", mediaType: "video/mp4"),
            Self.file("lineup.pdf", mediaType: "application/pdf"),
        ])
    }

    func testWithheldBesideText() {
        assertMessage(body: "Here is a photo from the match.", files: [Self.file("rink.jpg", mediaType: "image/jpeg")])
    }

    private func assertMessage(body: String = "", files: [SharedFile], testName: String = #function, line: UInt = #line) {
        let sample = SampleSession()
        let from = JID(parsing: "kai@away.example/mobile")!
        let conversation = ConversationID.direct(from.bare)
        let message = WireMessage(
            type: .chat, from: from, to: nil, identity: MessageIdentity(), body: body, sharedFiles: files
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
