import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport
#if canImport(ImageIO)
import ImageIO
import UniformTypeIdentifiers
#endif

struct MeActionOffsetTests {
    @Test func hidesThePrefixOfMeBodiesOnly() {
        #expect(MeAction.prefixScalarCount == 4)
        #expect(MeAction.hiddenScalarCount(ofBody: "/me waves") == 4)
        #expect(MeAction.hiddenScalarCount(ofBody: "/meshrugs") == 0)
        #expect(MeAction.hiddenScalarCount(ofBody: " /me waves") == 0)
    }

    /// Offsets stay over the full body; only the hidden prefix is cut off.
    @Test func keepsRangesAfterThePrefix() {
        // "/me waves at *bob*": bold span over "*bob*" at 13..<18.
        #expect(MeAction.visibleRange(13..<18, hiding: 4) == 13..<18)
        #expect(MeAction.visibleRange(4..<9, hiding: 4) == 4..<9)
    }

    @Test func clampsRangesThatStartInsideThePrefix() {
        #expect(MeAction.visibleRange(0..<9, hiding: 4) == 4..<9)
        #expect(MeAction.visibleRange(2..<5, hiding: 4) == 4..<5)
    }

    @Test func dropsRangesInsideThePrefix() {
        #expect(MeAction.visibleRange(0..<4, hiding: 4) == nil)
        #expect(MeAction.visibleRange(1..<3, hiding: 4) == nil)
    }

    @Test func noHiddenPrefixKeepsEveryRange() {
        #expect(MeAction.visibleRange(0..<3, hiding: 0) == 0..<3)
        #expect(MeAction.visibleRange(0..<3, hiding: -2) == 0..<3)
    }

    /// Emoji are one scalar each; the prefix is always four scalars.
    @Test func countsScalarsNotGraphemes() {
        let body = "/me 👋🏽 waves"
        #expect(MeAction.hiddenScalarCount(ofBody: body) == 4)
        let scalars = Array(body.unicodeScalars)
        let range = MeAction.visibleRange(0..<scalars.count, hiding: 4)
        #expect(range.map { String(String.UnicodeScalarView(scalars[$0])) } == "👋🏽 waves")
    }
}

struct GifMediaTests {
    @Test func recognisesGIFsByTypeExtensionOrGiphyHost() {
        let plain = URL(string: "https://upload.waddle.test/a/b")!
        #expect(GifMedia.isGIF(mediaType: "image/gif", url: plain))
        #expect(GifMedia.isGIF(mediaType: "IMAGE/GIF", url: plain))
        #expect(GifMedia.isGIF(mediaType: nil, url: URL(string: "https://upload.waddle.test/cat.GIF")!))
        #expect(GifMedia.isGIF(mediaType: nil, url: URL(string: "https://media3.giphy.com/media/abc/giphy.webp")!))
        #expect(!GifMedia.isGIF(mediaType: "image/png", url: plain))
        #expect(!GifMedia.isGIF(mediaType: nil, url: URL(string: "https://upload.waddle.test/cat.png")!))
    }

    @Test func giphyMediaHosts() {
        #expect(GifMedia.isGiphyMedia(URL(string: "https://media.giphy.com/media/x/giphy.gif")!))
        #expect(GifMedia.isGiphyMedia(URL(string: "https://media12.giphy.com/media/x/200.gif")!))
        #expect(GifMedia.isGiphyMedia(URL(string: "https://i.giphy.com/x.webp")!))
        #expect(!GifMedia.isGiphyMedia(URL(string: "https://giphy.com/gifs/x")!))
        #expect(!GifMedia.isGiphyMedia(URL(string: "https://mediax.giphy.com/media/x/giphy.gif")!))
        #expect(!GifMedia.isGiphyMedia(URL(string: "https://media.giphy.com.evil.test/x.gif")!))
        #expect(!GifMedia.isGiphyMedia(URL(string: "http://media.giphy.com/media/x/giphy.gif")!))
        #expect(!GifMedia.isGiphyMedia(URL(string: "https://i.giphy.com/")!))
    }

    @Test func recognisesGIFSignatures() {
        #expect(GifMedia.isGIF(data: Data("GIF89a\u{01}\u{00}".utf8)))
        #expect(GifMedia.isGIF(data: Data("GIF87a".utf8)))
        #expect(!GifMedia.isGIF(data: Data("GIF8".utf8)))
        #expect(!GifMedia.isGIF(data: Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])))
        #expect(!GifMedia.isGIF(data: Data()))
    }
}

#if canImport(ImageIO)
struct AnimatedImageMediaTests {
    @Test func gifAndPNGRequireMultipleFrames() throws {
        for type in [UTType.gif, .png] {
            let still = try encodedImage(type, frames: 1)
            let animation = try encodedImage(type, frames: 2)
            #expect(!AnimatedImageMedia.isAnimated(data: still))
            #expect(AnimatedImageMedia.isAnimated(data: animation))
        }
    }

    @Test func webPRequiresMultipleFrames() throws {
        // Two-pixel-square WebP files with one and two frames.
        let still = try #require(Data(base64Encoded: "UklGRhwAAABXRUJQVlA4TA8AAAAvAUAAAAcQ/Y/+ByKi/wEA"))
        let animation = try #require(Data(base64Encoded:
            "UklGRoQAAABXRUJQVlA4WAoAAAACAAAAAQAAAQAAQU5JTQYAAAD/////AABBTk1GKAAAAAAAAAAAAAEAAAEAAGQAAAJWUDhMDwAAAC8BQAAABxD9j/4HIqL/AQBBTk1GKAAAAAAAAAAAAAEAAAEAAGQAAABWUDhMDwAAAC8BQAAAB9D/iP4HIqL/AQA="
        ))
        #expect(!AnimatedImageMedia.isAnimated(data: still))
        #expect(AnimatedImageMedia.isAnimated(data: animation))
    }

    @Test func invalidDataAndMultiPageTIFFAreNotAnimations() throws {
        #expect(!AnimatedImageMedia.isAnimated(data: Data("GIF89a".utf8)))
        let tiff = try encodedImage(.tiff, frames: 2)
        let source = try #require(CGImageSourceCreateWithData(tiff as CFData, nil))
        #expect(CGImageSourceGetCount(source) == 2)
        #expect(!AnimatedImageMedia.isAnimated(data: tiff))
    }

    private func encodedImage(_ type: UTType, frames: Int) throws -> Data {
        let data = NSMutableData()
        let destination = try #require(CGImageDestinationCreateWithData(data, type.identifier as CFString, frames, nil))
        let context = try #require(CGContext(
            data: nil, width: 2, height: 2, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ))
        let properties: [CFString: Any] = switch type {
        case .gif: [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: 0.1]]
        case .png: [kCGImagePropertyPNGDictionary: [kCGImagePropertyAPNGDelayTime: 0.1]]
        default: [:]
        }
        for frame in 0..<frames {
            context.setFillColor(red: CGFloat(frame), green: 0, blue: 0, alpha: 1)
            context.fill(CGRect(x: 0, y: 0, width: 2, height: 2))
            CGImageDestinationAddImage(destination, try #require(context.makeImage()), properties as CFDictionary)
        }
        try #require(CGImageDestinationFinalize(destination))
        return data as Data
    }
}
#endif

struct InlineImageBodyTests {
    @Test func singleHTTPSImageURLs() {
        #expect(InlineImageBody.url(in: "https://media2.giphy.com/media/abc/giphy.gif")?.absoluteString == "https://media2.giphy.com/media/abc/giphy.gif")
        #expect(InlineImageBody.url(in: "  https://example.test/cat.JPG?size=large#top\n") != nil)
        #expect(InlineImageBody.url(in: "https://example.test/cat.jpeg") != nil)
        #expect(InlineImageBody.url(in: "https://example.test/cat.png") != nil)
        #expect(InlineImageBody.url(in: "https://example.test/cat.webp") != nil)
        #expect(InlineImageBody.url(in: "https://i.giphy.com/abc") != nil)
    }

    @Test func otherBodiesStayText() {
        let bodies = [
            "",
            "look https://example.test/cat.gif",
            "https://example.test/cat.gif and more",
            "https://example.test/cat.gif https://example.test/dog.gif",
            "http://example.test/cat.gif",
            "ftp://example.test/cat.gif",
            "https://example.test/cat.gif.html",
            "https://example.test/page",
            "https://example.test/cat.svg",
            "https://giphy.com/gifs/abc",
            "/me https://example.test/cat.gif",
            "example.test/cat.gif",
        ]
        for body in bodies {
            #expect(InlineImageBody.url(in: body) == nil, "\(body)")
        }
    }
}

struct AnimationFrameTimingTests {
    @Test func missingAndNearZeroDelaysPlayAtOneHundredMilliseconds() {
        #expect(AnimationFrameTiming.normalized(nil) == 0.1)
        #expect(AnimationFrameTiming.normalized(0) == 0.1)
        #expect(AnimationFrameTiming.normalized(0.01) == 0.1)
        #expect(AnimationFrameTiming.normalized(-1) == 0.1)
        #expect(AnimationFrameTiming.normalized(.nan) == 0.1)
        #expect(AnimationFrameTiming.normalized(0.02) == 0.02)
        #expect(AnimationFrameTiming.normalized(0.5) == 0.5)
        #expect(AnimationFrameTiming.normalized(655.35) == 10)
    }

    @Test func stepKeepsFramesWithinTheLimit() {
        #expect(AnimationFrameTiming.step(frameCount: 10, maxFrames: 200) == 1)
        #expect(AnimationFrameTiming.step(frameCount: 200, maxFrames: 200) == 1)
        #expect(AnimationFrameTiming.step(frameCount: 201, maxFrames: 200) == 2)
        #expect(AnimationFrameTiming.step(frameCount: 10, maxFrames: 3) == 4)
        #expect(AnimationFrameTiming.step(frameCount: 10, maxFrames: 0) == 10)
        #expect(AnimationFrameTiming.step(frameCount: 0, maxFrames: 5) == 1)
    }

    @Test func mergedDelaysKeepTheLoopLength() {
        let delays = [0.1, 0.2, 0.3, 0.4, 0.5]
        #expect(AnimationFrameTiming.mergedDelays(delays, step: 1) == delays)
        let merged = AnimationFrameTiming.mergedDelays(delays, step: 2)
        #expect(merged.count == 3)
        #expect(abs(merged.reduce(0, +) - 1.5) < 1e-9)
        #expect(abs(merged[2] - 0.5) < 1e-9)
        #expect(AnimationFrameTiming.mergedDelays([], step: 3).isEmpty)
    }

    @Test func playbackSlotsStayBoundedForAdversarialDelays() {
        // 10,000 frames at GIF's longest delay plus one 20 ms frame: the
        // common unit is 1 cs, which would need hundreds of millions of slots.
        let delays = Array(repeating: AnimationFrameTiming.normalized(655.35), count: 10_000) + [0.02]
        let step = AnimationFrameTiming.step(frameCount: delays.count, maxFrames: 200)
        let merged = AnimationFrameTiming.mergedDelays(delays, step: step)
        let playback = AnimationFrameTiming.playback(merged)
        let slots = playback.repeats.reduce(0, +)
        #expect(playback.repeats.count == merged.count)
        #expect(slots <= AnimationFrameTiming.maximumSlots + merged.count)
        #expect(playback.repeats.allSatisfy { $0 >= 1 })
        // Rounding moves each frame by at most half a slot.
        let length = merged.reduce(0, +)
        #expect(abs(playback.duration - length) <= playback.unit / 2 * Double(merged.count))
    }

    @Test func playbackUsesTheCommonCentisecondUnit() {
        let playback = AnimationFrameTiming.playback([0.1, 0.2, 0.05])
        #expect(playback == AnimationFrameTiming.Playback(unit: 0.05, repeats: [2, 4, 1]))
        #expect(abs(playback.duration - 0.35) < 1e-9)
        #expect(AnimationFrameTiming.playback([0.07, 0.1]) == AnimationFrameTiming.Playback(unit: 0.01, repeats: [7, 10]))
        #expect(AnimationFrameTiming.playback([]).repeats.isEmpty)
    }
}
