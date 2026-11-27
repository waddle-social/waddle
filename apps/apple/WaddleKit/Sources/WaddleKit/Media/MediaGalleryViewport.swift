import Foundation

/// The transform of the current image relative to its aspect-fit frame.
public struct MediaGalleryViewport: Equatable, Sendable {
    public private(set) var scale: CGFloat = 1
    public private(set) var offset: CGSize = .zero

    public init() {}

    public var isZoomed: Bool { scale > 1 }

    public func imageFrame(_ image: CGSize, in viewport: CGSize) -> CGRect {
        let size = CGSize(width: image.width * scale, height: image.height * scale)
        return CGRect(
            x: (viewport.width - size.width) / 2 + offset.width,
            y: (viewport.height - size.height) / 2 + offset.height,
            width: size.width,
            height: size.height
        )
    }

    public mutating func toggleZoom() {
        scale = isZoomed ? 1 : 2
        offset = .zero
    }

    public mutating func magnify(by factor: CGFloat, image: CGSize, in viewport: CGSize) {
        scale = min(5, max(1, scale * factor))
        pan(by: .zero, image: image, in: viewport)
    }

    public mutating func pan(by translation: CGSize, image: CGSize, in viewport: CGSize) {
        guard isZoomed else {
            offset = .zero
            return
        }
        let x = max(0, (image.width * scale - viewport.width) / 2)
        let y = max(0, (image.height * scale - viewport.height) / 2)
        offset = CGSize(
            width: min(x, max(-x, offset.width + translation.width)),
            height: min(y, max(-y, offset.height + translation.height))
        )
    }

    public func swipe(_ translation: CGSize) -> MediaGallery.Direction? {
        guard !isZoomed, abs(translation.width) >= 44, abs(translation.width) > abs(translation.height) else { return nil }
        return translation.width < 0 ? .next : .previous
    }
}
