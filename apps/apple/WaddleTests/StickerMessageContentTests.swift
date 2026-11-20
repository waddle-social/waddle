import WaddleKit
import XCTest
@testable import Waddle

/// What a row shows of a sticker message.
@MainActor
final class StickerMessageContentTests: XCTestCase {
    private let conversation = ConversationID.direct(SampleSession.priya)

    /// The text of a sticker message is for apps that do not draw
    /// stickers; here the picture stands for it, as on web and Android.
    func testAStickerShowsAsItsPictureWithoutItsText() async throws {
        let sample = SampleSession()
        let wave = Sticker(
            description: "Penguins",
            mediaType: "image/png",
            sources: [URL(string: "https://upload.waddle.test/stickers/wave.png")!]
        )

        await sample.coordinator.send(wave, from: StickerPack(id: "pack-1", stickers: [wave]), in: conversation)

        let row = try XCTUnwrap(sample.coordinator.timelines.timeline(for: conversation).items.last)
        XCTAssertEqual(row.body, "Penguins")
        XCTAssertNil(MessageContent.visibleBody(of: row))
    }

    func testAPictureSentWithWordsKeepsItsWords() async throws {
        let sample = SampleSession()
        let picture = SharedFile(url: URL(string: "https://upload.waddle.test/rink.png")!, mediaType: "image/png")

        await sample.coordinator.send(Draft(text: "First ice", attachments: [picture]), in: conversation)

        let row = try XCTUnwrap(sample.coordinator.timelines.timeline(for: conversation).items.last)
        XCTAssertEqual(MessageContent.visibleBody(of: row), "First ice")
    }
}
