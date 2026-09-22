import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers
import WaddleKit

/// Turns picked photo bytes into a square XEP-0084 PNG (the one content
/// type the XEP requires), downscaled to at most 512 × 512.
enum ProfileAvatarEncoder {
    enum Failure: LocalizedError {
        case unreadable
        case tooLarge

        var errorDescription: String? {
            switch self {
            case .unreadable: return "That photo could not be read. Try a different one."
            case .tooLarge: return "That photo is too detailed to use as an avatar. Try a different one."
            }
        }
    }

    /// Upper bound for the decoded source edge, to keep memory bounded for
    /// very large photos before cropping.
    private static let decodeLimit = 2_048

    static func encode(_ data: Data) throws -> AvatarImage {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let oriented = orientedImage(from: source),
              let square = centerSquare(of: oriented)
        else { throw Failure.unreadable }

        for side in ProfileAvatarSizing.candidateSides(forSourceSide: square.width) {
            guard let scaled = render(square, side: side), let png = pngData(of: scaled) else { continue }
            if png.count <= ProfileAvatarSizing.maxBytes {
                return AvatarImage(data: png, mediaType: "image/png", width: side, height: side)
            }
        }
        throw Failure.tooLarge
    }

    /// Decodes the first frame with its EXIF orientation applied.
    private static func orientedImage(from source: CGImageSource) -> CGImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: min(longestEdge(of: source) ?? decodeLimit, decodeLimit),
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    private static func longestEdge(of source: CGImageSource) -> Int? {
        guard let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int
        else { return nil }
        return max(width, height)
    }

    private static func centerSquare(of image: CGImage) -> CGImage? {
        let crop = ProfileAvatarSizing.centeredSquare(width: image.width, height: image.height)
        guard crop.side > 0 else { return nil }
        let rect = CGRect(x: crop.x, y: crop.y, width: crop.side, height: crop.side)
        return image.cropping(to: rect)
    }

    private static func render(_ image: CGImage, side: Int) -> CGImage? {
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(
                  data: nil,
                  width: side,
                  height: side,
                  bitsPerComponent: 8,
                  bytesPerRow: 0,
                  space: space,
                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
              )
        else { return nil }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: side, height: side))
        return context.makeImage()
    }

    private static func pngData(of image: CGImage) -> Data? {
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            output as CFMutableData,
            UTType.png.identifier as CFString,
            1,
            nil
        ) else { return nil }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return output as Data
    }
}
