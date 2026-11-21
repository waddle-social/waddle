import AVFoundation
import XCTest
@testable import Waddle

/// Picking a clip for a story: what is made ready to upload, and what is
/// refused. The clips are written here, so the real export runs.
final class StoryComposerTests: ClipTestCase {
    func testAPickedClipIsSharedAsAnMP4WithoutWhereItWasFilmed() async throws {
        let source = try await clip(filmedAt: "+59.9139+010.7522/")
        let composer = StoryComposer()

        await composer.pickVideo { source }

        let media = try XCTUnwrap(composer.media, composer.errorMessage ?? "")
        XCTAssertTrue(media.isVideo)
        XCTAssertEqual(media.payload.mediaType, "video/mp4")
        XCTAssertNotNil(composer.preview)
        XCTAssertTrue(composer.canPost)
        let codec = try await videoCodec(of: media.payload.data)
        XCTAssertEqual(codec, kCMVideoCodecType_H264)
        let locations = try await locations(in: media.payload.data)
        XCTAssertEqual(locations, [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: source.path), "The picked copy is removed")
    }

    func testAClipOverTheCapIsRefusedSayingTheCap() async throws {
        let source = try await clip()
        let composer = StoryComposer(maxBytes: 1024)

        await composer.pickVideo { source }

        XCTAssertNil(composer.media)
        XCTAssertEqual(composer.errorMessage, "Videos in a story can be up to 1 KB.")
        XCTAssertFalse(composer.canPost)
    }

    func testTheCapReadsAsPeopleWriteIt() {
        XCTAssertEqual(StoryComposer().limitText, "10 MB")
    }

    func testAFileThatIsNotAVideoIsRefused() async throws {
        let source = temporaryFile("mov")
        try Data("not a movie".utf8).write(to: source)
        let composer = StoryComposer()

        await composer.pickVideo { source }

        XCTAssertNil(composer.media)
        XCTAssertEqual(composer.errorMessage, "Couldn't use that video. Try another one.")
    }
}
