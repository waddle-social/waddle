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

    func testASecondPickCancelsTheConversionOfTheFirst() async throws {
        let first = temporaryFile("mov")
        let started = expectation(description: "The first conversion started")
        let cancelled = expectation(description: "The first conversion was cancelled")
        let composer = StoryComposer(prepareClip: { source, limit in
            guard source == first else { return try await StoryClip.prepare(source, maxBytes: limit) }
            started.fulfill()
            do {
                try await Task.sleep(for: .seconds(60))
            } catch {
                cancelled.fulfill()
                throw error
            }
            XCTFail("The first conversion ran on")
            throw CancellationError()
        })
        let firstPick = Task { await composer.pickVideo { first } }
        await fulfillment(of: [started], timeout: 5)

        let second = try await clip()
        await composer.pickVideo { second }

        await fulfillment(of: [cancelled], timeout: 5)
        await firstPick.value
        XCTAssertNotNil(composer.media)
        XCTAssertNil(composer.errorMessage)
        XCTAssertFalse(composer.isPicking)
    }

    func testClosingTheSheetCancelsTheConversion() async throws {
        let started = expectation(description: "The conversion started")
        let cancelled = expectation(description: "The conversion was cancelled")
        let composer = StoryComposer(prepareClip: { _, _ in
            started.fulfill()
            do {
                try await Task.sleep(for: .seconds(60))
            } catch {
                cancelled.fulfill()
                throw error
            }
            throw CancellationError()
        })
        let pick = Task { await composer.pickVideo { URL(fileURLWithPath: "/dev/null") } }
        await fulfillment(of: [started], timeout: 5)

        composer.cancelPicking()

        await fulfillment(of: [cancelled], timeout: 5)
        await pick.value
        XCTAssertNil(composer.media)
        XCTAssertNil(composer.errorMessage)
        XCTAssertFalse(composer.isPicking)
    }
}

