import Foundation
#if canImport(ImageIO)
import ImageIO
#endif

/// Image metadata and re-encoding for attachments, through ImageIO so it
/// works the same on iOS and macOS.
enum AttachmentImageInfo {
    /// Pixel size as displayed (EXIF orientations 5–8 swap the axes).
    static func pixelSize(of data: Data) -> (width: Int, height: Int)? {
        #if canImport(ImageIO)
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int
        else { return nil }
        if let orientation = properties[kCGImagePropertyOrientation] as? Int, (5...8).contains(orientation) {
            return (height, width)
        }
        return (width, height)
        #else
        return nil
        #endif
    }

    /// Re-encodes a photo as JPEG from its pixels, without any source
    /// metadata, so a shared photo does not leak where it was taken and
    /// HEIC reaches clients that cannot decode it.
    static func sanitizedJPEG(from data: Data, quality: Double = 0.85) -> Data? {
        #if canImport(ImageIO)
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
        else { return nil }
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output as CFMutableData, "public.jpeg" as CFString, 1, nil) else {
            return nil
        }
        // Pixels only: EXIF, XMP, IPTC and maker notes are not copied, so no
        // location survives in any of them. Orientation is kept so the
        // photo displays upright.
        var options: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: quality]
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        if let orientation = properties?[kCGImagePropertyOrientation] {
            options[kCGImagePropertyOrientation] = orientation
        }
        CGImageDestinationAddImage(destination, image, options as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return output as Data
        #else
        return nil
        #endif
    }

    /// Whether the image carries GPS metadata (EXIF, XMP-derived or PNG
    /// eXIf chunks all surface here through ImageIO).
    static func hasLocation(_ data: Data) -> Bool {
        #if canImport(ImageIO)
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else { return false }
        return properties[kCGImagePropertyGPSDictionary] != nil
        #else
        return false
        #endif
    }

    /// A small JPEG for the composer chip.
    static func thumbnail(from data: Data, maxPixelSize: Int = 160) -> Data? {
        #if canImport(ImageIO)
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output as CFMutableData, "public.jpeg" as CFString, 1, nil) else {
            return nil
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return output as Data
        #else
        return nil
        #endif
    }
}
