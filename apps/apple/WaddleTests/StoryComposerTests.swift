import AVFoundation
import XCTest
@testable import Waddle

/// Picking a clip for a story: what is made ready to upload, and what is
/// refused. The clip is written here with AVFoundation, so the real export
/// runs.
@MainActor
final class StoryComposerTests: XCTestCase {
    private var files: [URL] = []

    override func tearDown() async throws {
        for file in files { try? FileManager.default.removeItem(at: file) }
        try await super.tearDown()
    }

    func testAPickedClipIsSharedAsAnMP4WithoutWhereItWasFilmed() async throws {
        let source = try await clip(filmedAt: "+59.9139+010.7522/")
        let composer = StoryComposer()

        await composer.pickVideo { source }

        let media = try XCTUnwrap(composer.media, composer.errorMessage ?? "")
        XCTAssertEqual(media.mediaType, "video/mp4")
        XCTAssertTrue(composer.isVideo)
        XCTAssertNotNil(composer.preview)
        XCTAssertTrue(composer.canPost)
        let shared = file("mp4")
        try media.data.write(to: shared)
        let asset = AVURLAsset(url: shared)
        let tracks = try await asset.loadTracks(withMediaType: .video)
        XCTAssertEqual(tracks.count, 1)
        let locations = try await asset.load(.metadata).filter {
            $0.identifier == .quickTimeMetadataLocationISO6709 || $0.commonKey == .commonKeyLocation
        }
        XCTAssertEqual(locations, [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: source.path), "The picked copy is removed")
    }

    func testAClipOver10MBIsRefused() async throws {
        let source = file("mov")
        try Data(count: StoryComposer.maxBytes + 1).write(to: source)
        let composer = StoryComposer()

        await composer.pickVideo { source }

        XCTAssertNil(composer.media)
        XCTAssertEqual(composer.errorMessage, "Videos in a story can be up to 10 MB.")
        XCTAssertFalse(composer.canPost)
    }

    func testAFileThatIsNotAVideoIsRefused() async throws {
        let source = file("mov")
        try Data("not a movie".utf8).write(to: source)
        let composer = StoryComposer()

        await composer.pickVideo { source }

        XCTAssertNil(composer.media)
        XCTAssertEqual(composer.errorMessage, "Couldn't use that video. Try another one.")
    }

    // MARK: - Clips

    private func file(_ fileExtension: String) -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("story-composer-test-\(UUID().uuidString)")
            .appendingPathExtension(fileExtension)
        files.append(url)
        return url
    }

    /// A second of 64×64 H.264 in a QuickTime file that says where it was
    /// filmed.
    private func clip(filmedAt location: String) async throws -> URL {
        let url = file("mov")
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let place = AVMutableMetadataItem()
        place.identifier = .quickTimeMetadataLocationISO6709
        place.dataType = kCMMetadataBaseDataType_UTF8 as String
        place.value = location as NSString
        writer.metadata = [place]
        let size = 64
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: size, AVVideoHeightKey: size,
        ])
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: size,
            kCVPixelBufferHeightKey as String: size,
        ])
        writer.add(input)
        XCTAssertTrue(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<10 {
            while !input.isReadyForMoreMediaData {
                try await Task.sleep(for: .milliseconds(5))
            }
            var buffer: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, try XCTUnwrap(adaptor.pixelBufferPool), &buffer)
            let pixels = try XCTUnwrap(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            memset(CVPixelBufferGetBaseAddress(pixels), 0x60, CVPixelBufferGetDataSize(pixels))
            CVPixelBufferUnlockBaseAddress(pixels, [])
            adaptor.append(pixels, withPresentationTime: CMTime(value: CMTimeValue(frame), timescale: 10))
        }
        input.markAsFinished()
        await writer.finishWriting()
        XCTAssertEqual(writer.status, .completed, writer.error.map { "\($0)" } ?? "")
        let written = try await AVURLAsset(url: url).load(.metadata)
        XCTAssertFalse(written.isEmpty, "The clip says where it was filmed")
        return url
    }
}
