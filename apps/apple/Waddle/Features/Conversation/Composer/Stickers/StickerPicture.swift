import Foundation
import ImageIO

/// Turns a chosen picture into a sticker's picture: no larger than 512
/// pixels on its longest side, drawn from its pixels alone so nothing
/// about where or how it was taken goes with it.
enum StickerPicture {
    static let maxPixelSize = 512
    /// A small GIF keeps its animation; a larger one becomes a still.
    static let maxAnimatedBytes = 1024 * 1024

    /// Nil when the data is not a picture.
    static func payload(from data: Data, mediaType: String?) -> AttachmentPayload? {
        if let animated = animatedPayload(from: data, mediaType: mediaType) {
            return animated
        }
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        let opaque: [CGImageAlphaInfo] = [.none, .noneSkipFirst, .noneSkipLast]
        // Transparency is most of what makes a sticker; keep it.
        let format = opaque.contains(image.alphaInfo)
            ? (type: "public.jpeg", mediaType: "image/jpeg", fileExtension: "jpg")
            : (type: "public.png", mediaType: "image/png", fileExtension: "png")
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output as CFMutableData, format.type as CFString, 1, nil) else {
            return nil
        }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.85] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return named(output as Data, mediaType: format.mediaType, fileExtension: format.fileExtension, width: image.width, height: image.height)
    }

    /// A small GIF, written again from its frames and their timing alone:
    /// comments and any other text a GIF can hold out of sight are left
    /// behind. Nil for anything else, which then becomes a still.
    private static func animatedPayload(from data: Data, mediaType: String?) -> AttachmentPayload? {
        guard mediaType?.lowercased() == "image/gif",
              data.count <= maxAnimatedBytes,
              let size = AttachmentImageInfo.pixelSize(of: data),
              max(size.width, size.height) <= maxPixelSize,
              let source = CGImageSourceCreateWithData(data as CFData, nil)
        else { return nil }
        let frameCount = CGImageSourceGetCount(source)
        let output = NSMutableData()
        guard frameCount > 1,
              let destination = CGImageDestinationCreateWithData(output as CFMutableData, "com.compuserve.gif" as CFString, frameCount, nil)
        else { return nil }
        let loopCount = gifProperties(CGImageSourceCopyProperties(source, nil))[kCGImagePropertyGIFLoopCount] ?? 0
        CGImageDestinationSetProperties(
            destination, [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFLoopCount: loopCount]] as CFDictionary
        )
        for index in 0..<frameCount {
            guard let frame = CGImageSourceCreateImageAtIndex(source, index, nil) else { return nil }
            let timing = gifProperties(CGImageSourceCopyPropertiesAtIndex(source, index, nil))
            let delay = timing[kCGImagePropertyGIFUnclampedDelayTime] ?? timing[kCGImagePropertyGIFDelayTime] ?? 0.1
            CGImageDestinationAddImage(
                destination, frame, [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: delay]] as CFDictionary
            )
        }
        guard CGImageDestinationFinalize(destination), output.length <= maxAnimatedBytes else { return nil }
        return named(output as Data, mediaType: "image/gif", fileExtension: "gif", width: size.width, height: size.height)
    }

    private static func gifProperties(_ properties: CFDictionary?) -> [CFString: Any] {
        (properties as? [CFString: Any])?[kCGImagePropertyGIFDictionary] as? [CFString: Any] ?? [:]
    }

    /// Anyone sent the sticker can fetch its picture, so it does not carry
    /// the name it had on this device.
    private static func named(_ data: Data, mediaType: String, fileExtension: String, width: Int, height: Int) -> AttachmentPayload {
        AttachmentPayload(
            data: data,
            filename: "sticker-\(UUID().uuidString.lowercased()).\(fileExtension)",
            mediaType: mediaType,
            width: width,
            height: height
        )
    }
}
