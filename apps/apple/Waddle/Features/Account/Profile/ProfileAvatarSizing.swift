import Foundation

/// Size policy for published XEP-0084 avatars.
enum ProfileAvatarSizing {
    /// Largest edge we publish.
    static let maxSide = 512
    /// The PNG travels base64-encoded inside one pubsub stanza; staying
    /// under this keeps it far below the 1 MiB XMPP frame limit.
    static let maxBytes = 256 * 1024
    /// Smaller edges tried in turn when a PNG is over `maxBytes`.
    static let fallbackSides = [384, 256, 192, 128, 96, 64]

    /// Square edge lengths to try for a square source of `sourceSide`
    /// pixels, largest first. Never upscales.
    static func candidateSides(forSourceSide sourceSide: Int) -> [Int] {
        guard sourceSide > 0 else { return [] }
        let first = min(sourceSide, maxSide)
        return [first] + fallbackSides.filter { $0 < first }
    }

    /// The centered square crop of a `width` × `height` image.
    static func centeredSquare(width: Int, height: Int) -> (x: Int, y: Int, side: Int) {
        let side = min(width, height)
        return ((width - side) / 2, (height - side) / 2, side)
    }
}
