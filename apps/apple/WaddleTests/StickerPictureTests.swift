import ImageIO
import XCTest
@testable import Waddle

/// What a chosen picture carries once it is a sticker's picture.
final class StickerPictureTests: XCTestCase {
    /// A GIF can hold text nobody sees (a comment block). A sticker's
    /// picture is public, so only its frames and their timing are kept.
    func testAnAnimatedStickerKeepsItsFramesAndDropsHiddenText() throws {
        let secret = Data("made by ana on her own laptop".utf8)
        let gif = Self.withComment(secret, in: Self.gif(frames: 3))
        XCTAssertNotNil(gif.range(of: secret))

        let payload = try XCTUnwrap(StickerPicture.payload(from: gif, mediaType: "image/gif"))

        XCTAssertEqual(payload.mediaType, "image/gif")
        XCTAssertNil(payload.data.range(of: secret))
        let source = try XCTUnwrap(CGImageSourceCreateWithData(payload.data as CFData, nil))
        XCTAssertEqual(CGImageSourceGetCount(source), 3)
        XCTAssertEqual(Self.delay(ofFrame: 1, in: source), 0.2, accuracy: 0.001)
        XCTAssertEqual(payload.width, 32)
        XCTAssertEqual(payload.height, 32)
    }

    func testAStillStickerIsNoLargerThan512Pixels() throws {
        let payload = try XCTUnwrap(StickerPicture.payload(from: Self.png(width: 2048, height: 1024), mediaType: "image/png"))

        XCTAssertEqual(payload.width, 512)
        XCTAssertEqual(payload.height, 256)
    }

    // MARK: - Pictures drawn here

    private static func frame(width: Int, height: Int, shade: CGFloat) -> CGImage {
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        context.setFillColor(red: shade, green: 0.33, blue: 0.48, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()!
    }

    private static func png(width: Int, height: Int) -> Data {
        let data = NSMutableData()
        let destination = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, frame(width: width, height: height, shade: 0.1), nil)
        CGImageDestinationFinalize(destination)
        return data as Data
    }

    private static func gif(frames: Int) -> Data {
        let data = NSMutableData()
        let destination = CGImageDestinationCreateWithData(data, "com.compuserve.gif" as CFString, frames, nil)!
        let timing = [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: 0.2]] as CFDictionary
        for index in 0..<frames {
            CGImageDestinationAddImage(destination, frame(width: 32, height: 32, shade: CGFloat(index) / 4), timing)
        }
        CGImageDestinationFinalize(destination)
        return data as Data
    }

    /// Adds a GIF comment extension (0x21 0xFE, one sub-block, a
    /// terminator) just before the trailer byte.
    private static func withComment(_ text: Data, in gif: Data) -> Data {
        var bytes = gif
        let trailer = bytes.removeLast()
        bytes.append(contentsOf: [0x21, 0xFE, UInt8(text.count)])
        bytes.append(text)
        bytes.append(contentsOf: [0x00, trailer])
        return bytes
    }

    private static func delay(ofFrame index: Int, in source: CGImageSource) -> Double {
        let properties = CGImageSourceCopyPropertiesAtIndex(source, index, nil) as? [CFString: Any]
        let gif = properties?[kCGImagePropertyGIFDictionary] as? [CFString: Any]
        return gif?[kCGImagePropertyGIFDelayTime] as? Double ?? 0
    }
}
