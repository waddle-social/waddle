import SwiftUI
import WaddleKit
import XCTest

@testable import Waddle

@MainActor
final class MessageAttachmentPlayerSnapshotTests: XCTestCase {
    func testIdleAudio() {
        assertCard(SampleSession().host(
            InlineMediaPlayer(file: Self.file("Practice notes.mp3"), medium: .audio)
        ))
    }

    func testIdleVideo() {
        assertCard(SampleSession().host(
            InlineMediaPlayer(file: Self.file("Final lap.mp4"), medium: .video)
        ))
    }

    func testPDFLoading() {
        assertCard(PDFAttachmentCard(file: Self.pdf, phase: .loading, open: {}))
    }

    func testPDFLoaded() {
        assertCard(PDFAttachmentCard(file: Self.pdf, phase: .loaded(Self.page, size: 25_600), open: {}))
    }

    func testPDFFailed() {
        assertCard(PDFAttachmentCard(file: Self.pdf, phase: .failed, open: {}))
    }

    #if os(iOS)
    func testPDFLoadedOnPhone() {
        let card = PDFAttachmentCard(file: Self.pdf, phase: .loaded(Self.page, size: 25_600), open: {})
        assertHuddleSnapshot(of: card.environment(\.horizontalSizeClass, .compact))
    }
    #endif

    func testWithheldAudioVideoAndPDF() {
        let from = JID(parsing: "kai@away.example/mobile")!
        let conversation = ConversationID.direct(from.bare)
        let message = WireMessage(
            type: .chat, from: from, to: nil, identity: MessageIdentity(), body: "",
            sharedFiles: [Self.file("Voice.mp3"), Self.file("Lap.mp4"), Self.pdf]
        )
        let item = TimelineItem(id: "withheld", conversation: conversation, isMine: false, message: message, body: "")
        let entry = TimelineFeedEntry(
            item: item, daySeparator: nil, unreadDivider: nil, startsGroup: false, thread: nil, replyParent: nil
        )
        let actions = MessageActionModel(conversation: conversation, composer: ComposerModel(), thread: nil)
        assertCard(SampleSession().host(MessageRow(entry: entry) { _ in }.environment(actions)))
    }

    private func assertCard(_ card: some View, testName: String = #function, line: UInt = #line) {
        #if os(iOS)
        assertHuddleSnapshot(of: card.environment(\.horizontalSizeClass, .regular), testName: testName, line: line)
        #else
        assertHuddleSnapshot(of: card, testName: testName, line: line)
        #endif
    }

    private static func file(_ name: String) -> SharedFile {
        let base = URL(string: "https://files.away.example/")!
        return SharedFile(url: base.appendingPathComponent(name), name: name, size: 25_600, disposition: .inline)
    }

    private static let pdf = file("Training schedule.pdf")

    private static let page: Image = {
        let context = CGContext(
            data: nil, width: 150, height: 200, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        context.setFillColor(red: 1, green: 1, blue: 1, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: 150, height: 200))
        context.setFillColor(red: 0.16, green: 0.30, blue: 0.46, alpha: 1)
        context.fill(CGRect(x: 16, y: 166, width: 84, height: 8))
        context.setFillColor(red: 0.70, green: 0.74, blue: 0.78, alpha: 1)
        for y in stride(from: 145, through: 65, by: -12) {
            context.fill(CGRect(x: 16, y: y, width: y.isMultiple(of: 5) ? 88 : 118, height: 3))
        }
        return Image(decorative: context.makeImage()!, scale: 1)
    }()
}
