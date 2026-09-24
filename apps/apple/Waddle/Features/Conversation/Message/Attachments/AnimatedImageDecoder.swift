import Foundation
import ImageIO
import WaddleKit
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// Image bytes decoded for display. On iOS an animated GIF becomes a UIKit
/// animated image; on macOS `NSImageView` plays the GIF itself.
struct DecodedAnimatedImage: @unchecked Sendable {
    #if os(iOS)
    let image: UIImage
    #elseif os(macOS)
    let image: NSImage
    #endif
    /// Aspect ratio source for layout; the natural size when unconstrained.
    let size: CGSize
}

/// Decodes GIF (and still image) bytes. iOS frames are downsampled and
/// capped by a memory budget, dropping evenly spaced frames when a GIF has
/// more than the budget holds, so a long GIF keeps its loop length.
enum AnimatedImageDecoder {
    /// Twice the timeline's media width, in pixels.
    static let defaultMaxPixelSize: CGFloat = 720
    static let frameLimit = 200
    static let byteBudget = 16 * 1024 * 1024

    /// Decodes off the main actor.
    static func decodeInBackground(_ data: Data, animated: Bool, maxPixelSize: CGFloat) async -> DecodedAnimatedImage? {
        await Task.detached(priority: .userInitiated) {
            AnimatedImageDecoder.decode(data, animated: animated, maxPixelSize: maxPixelSize)
        }.value
    }

    static func decode(_ data: Data, animated: Bool, maxPixelSize: CGFloat) -> DecodedAnimatedImage? {
        #if os(iOS)
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let count = CGImageSourceGetCount(source)
        guard count > 0, let first = frame(source, at: 0, maxPixelSize: maxPixelSize) else { return nil }
        let still = UIImage(cgImage: first)
        guard animated, count > 1 else {
            return DecodedAnimatedImage(image: still, size: still.size)
        }
        let frameBytes = max(first.bytesPerRow * first.height, 1)
        let maxFrames = min(frameLimit, max(byteBudget / frameBytes, 1))
        let step = AnimationFrameTiming.step(frameCount: count, maxFrames: maxFrames)
        let delays = (0..<count).map { AnimationFrameTiming.normalized(delay(source, at: $0)) }
        var frames: [UIImage] = [still]
        for index in stride(from: step, to: count, by: step) {
            // A frame that fails to decode repeats the previous one so the
            // timing stays aligned.
            if let image = frame(source, at: index, maxPixelSize: maxPixelSize) {
                frames.append(UIImage(cgImage: image))
            } else {
                frames.append(frames[frames.count - 1])
            }
        }
        let playback = AnimationFrameTiming.playback(AnimationFrameTiming.mergedDelays(delays, step: step))
        var slots: [UIImage] = []
        for (image, repeats) in zip(frames, playback.repeats) {
            slots.append(contentsOf: repeatElement(image, count: repeats))
        }
        guard slots.count > 1, let animation = UIImage.animatedImage(with: slots, duration: playback.duration) else {
            return DecodedAnimatedImage(image: still, size: still.size)
        }
        return DecodedAnimatedImage(image: animation, size: still.size)
        #elseif os(macOS)
        guard let image = NSImage(data: data) else { return nil }
        return DecodedAnimatedImage(image: image, size: image.size)
        #endif
    }

    #if os(iOS)
    /// One composited frame, downsampled to `maxPixelSize`.
    private static func frame(_ source: CGImageSource, at index: Int, maxPixelSize: CGFloat) -> CGImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, index, options as CFDictionary)
    }

    /// The frame's declared delay: GIF, WebP or APNG, unclamped first.
    private static func delay(_ source: CGImageSource, at index: Int) -> Double? {
        guard let properties = CGImageSourceCopyPropertiesAtIndex(source, index, nil) as? [CFString: Any] else {
            return nil
        }
        let formats: [(dictionary: CFString, unclamped: CFString, clamped: CFString)] = [
            (kCGImagePropertyGIFDictionary, kCGImagePropertyGIFUnclampedDelayTime, kCGImagePropertyGIFDelayTime),
            (kCGImagePropertyWebPDictionary, kCGImagePropertyWebPUnclampedDelayTime, kCGImagePropertyWebPDelayTime),
            (kCGImagePropertyPNGDictionary, kCGImagePropertyAPNGUnclampedDelayTime, kCGImagePropertyAPNGDelayTime),
        ]
        for format in formats {
            guard let values = properties[format.dictionary] as? [CFString: Any] else { continue }
            if let unclamped = (values[format.unclamped] as? NSNumber)?.doubleValue, unclamped > 0 {
                return unclamped
            }
            return (values[format.clamped] as? NSNumber)?.doubleValue
        }
        return nil
    }
    #endif
}
