import AVFoundation
import XCTest
@testable import Waddle

/// Which clips are copied as they are and which are converted to H.264,
/// so web and Android can play every story.
final class StoryClipTests: ClipTestCase {
    func testOnlyH264WithAACOrNoSoundIsCopiedAsItIs() {
        XCTAssertEqual(StoryClip.encoding(video: kCMVideoCodecType_H264, audio: []), .copy)
        XCTAssertEqual(StoryClip.encoding(video: kCMVideoCodecType_H264, audio: [kAudioFormatMPEG4AAC]), .copy)
        XCTAssertEqual(StoryClip.encoding(video: kCMVideoCodecType_HEVC, audio: [kAudioFormatMPEG4AAC]), .convert)
        XCTAssertEqual(StoryClip.encoding(video: kCMVideoCodecType_H264, audio: [kAudioFormatLinearPCM]), .convert)
    }

    /// Skipped where the machine has no HEVC encoder.
    func testAnHEVCClipIsConvertedToH264WithoutWhereItWasFilmed() async throws {
        let source = try await clip(codec: .hevc, width: 320, height: 240, filmedAt: "+59.9139+010.7522/")

        let prepared = try await StoryClip.prepare(source, maxBytes: StoryComposer.storyLimit)

        XCTAssertEqual(prepared.payload.mediaType, "video/mp4")
        let codec = try await videoCodec(of: prepared.payload.data)
        XCTAssertEqual(codec, kCMVideoCodecType_H264)
        let locations = try await locations(in: prepared.payload.data)
        XCTAssertEqual(locations, [])
    }

    /// The cap applies to the converted file, not to what was picked.
    func testAConvertedClipOverTheCapIsRefused() async throws {
        let source = try await clip(codec: .hevc, width: 320, height: 240)

        do {
            _ = try await StoryClip.prepare(source, maxBytes: 64)
            XCTFail("A clip over the cap was prepared")
        } catch StoryClip.Failure.tooLarge {}
    }
}
