import CryptoKit
import SwiftUI

/// The app's design vocabulary: Polar Huddle tokens (`WaddleTokens`,
/// generated from `design/src/preset.ts`) under the names views use, plus
/// the app's own layout sizes.
enum Theme {
    /// Semantic colours; each follows the Night or Daylight appearance.
    typealias Colors = WaddleTokens.Colors

    /// Interaction layers (web `state.*`): the foreground over whatever
    /// ground the control sits on.
    enum State {
        static let hover = Colors.fg.opacity(0.06)
        static let selected = Colors.fg.opacity(0.10)
        static let press = Colors.fg.opacity(0.10)
    }

    /// Text styles, applied with `.huddleText(_:)`.
    typealias Typography = WaddleTokens.TextStyles

    /// The app's own text styles, for things the preset has no style for.
    enum AppTypography {
        /// The device code on sign-in: large monospaced characters, spaced
        /// so each reads on its own.
        static let signInCode = HuddleTextStyle(font: .mono, size: 30, weight: 600, lineHeight: 1.2, letterSpacing: 0.08)
    }

    enum Spacing {
        static let xxs = WaddleTokens.Spacing.s0Half
        static let xs = WaddleTokens.Spacing.s1
        static let s = WaddleTokens.Spacing.s2
        static let m = WaddleTokens.Spacing.s3
        static let l = WaddleTokens.Spacing.s4
        static let xl = WaddleTokens.Spacing.s6
    }

    /// Concentric radii: an inner radius is the outer one minus the
    /// padding between them, never below `small`.
    enum Radius {
        /// Tags and inline code.
        static let small = WaddleTokens.Radius.tag
        /// Buttons, fields and rows.
        static let medium = WaddleTokens.Radius.control
        /// Panels and popovers.
        static let large = WaddleTokens.Radius.panel
        /// Cards.
        static let card = WaddleTokens.Radius.card
        static let bubble = WaddleTokens.Radius.bubble
        /// Sheets and dialogs.
        static let sheet = WaddleTokens.Radius.sheet
    }

    enum Size {
        static let avatar: CGFloat = 36
        static let smallAvatar: CGFloat = 24
        static let rowAvatar: CGFloat = 28
        /// The person on a call, beside its controls.
        static let callAvatar: CGFloat = 44
        /// The person on a call, alone on the video stage.
        static let callStageAvatar: CGFloat = 72
        /// Max width for message media so it never spans a wide Mac window.
        static let mediaMaxWidth: CGFloat = 360
        /// The accent rail on a row's or chip's leading edge (web's 3px
        /// inset rails: a mention, a reply quote, a block quote).
        static let rail: CGFloat = 3
        /// The large mark of an empty conversation.
        static let emptyMark: CGFloat = 64
        /// The brand mark over the sign-in card.
        static let signInMark: CGFloat = 88
        /// The sign-in column (web `signin-many` card, less its padding).
        static let signInColumn: CGFloat = 360
        /// A small dot for a live or reachable thing, such as the server
        /// on sign-in.
        static let liveDot: CGFloat = 8
        /// The preview on a settings tile (Appearance, Message order).
        static let settingsPreview: CGFloat = 64
        /// A bar in a settings tile's preview: a message, the composer.
        static let previewBar: CGFloat = 6
        /// The narrowest emoji tile in a mood or activity picker.
        static let choiceTile: CGFloat = 76
        /// The person a profile or DM details is about.
        static let profileAvatar: CGFloat = 88
        /// A person on a room's member list (Android `AvatarSize.LG`).
        static let memberAvatar: CGFloat = 44
        /// The glyph column of a details or settings row.
        static let rowGlyph: CGFloat = 20
        /// The emoji or glyph beside something someone shares.
        static let sharingGlyph: CGFloat = 32
        /// A form row (details, settings, profile): a touch target on iPhone, a control
        /// on Mac.
        static var formRow: CGFloat {
            #if os(iOS)
            WaddleTokens.ControlHeight.touch
            #else
            WaddleTokens.ControlHeight.sm
            #endif
        }
        /// Smallest tap target (Human Interface Guidelines).
        static let minHitTarget = WaddleTokens.ControlHeight.touch
    }

    /// Consecutive messages from one author within this window share one
    /// header.
    static let groupingWindow: TimeInterval = 5 * 60

    static let quickReactions = ["👍", "❤️", "😂", "🎉", "👀", "🙏"]
}

extension Color {
    /// XEP-0392 consistent color for `identifier`, from the shared Rust
    /// implementation so every Waddle client picks the same hue.
    static func consistent(for identifier: String) -> Color {
        // Waddle renders HSL 55%/45% on every client; this is the same
        // color in SwiftUI's HSB space.
        Color(hue: consistentColorHue(input: identifier) / 360, saturation: 0.7097, brightness: 0.6975)
    }

    /// The page ground.
    static var primaryBackground: Color { Theme.Colors.bg }

    /// A recessed or grouped area on the page ground.
    static var secondaryBackground: Color { Theme.Colors.surface2 }

    /// Hairline border of raised cards.
    static var cardBorder: Color { Theme.Colors.borderSoft }
}

#if os(macOS)
private typealias PlatformImage = NSImage
#else
private typealias PlatformImage = UIImage
#endif

@MainActor
private enum DecodedImageCache {
    static let cache: NSCache<NSString, PlatformImage> = {
        let cache = NSCache<NSString, PlatformImage>()
        cache.countLimit = 128
        cache.totalCostLimit = 16 * 1024 * 1024
        return cache
    }()

    static func image(for data: Data) -> PlatformImage? {
        let digest = SHA256.hash(data: data)
        let key = digest.compactMap { String(format: "%02x", $0) }.joined() as NSString
        if let cached = cache.object(forKey: key) {
            return cached
        }
        guard let decoded = PlatformImage(data: data) else { return nil }
        cache.setObject(decoded, forKey: key, cost: data.count)
        return decoded
    }
}

extension Image {
    /// Decodes image bytes on either platform.
    init?(data: Data) {
        guard let image = PlatformImage(data: data) else { return nil }
        #if os(macOS)
        self.init(nsImage: image)
        #else
        self.init(uiImage: image)
        #endif
    }

    /// Decodes avatar bytes through a bounded main-actor cache so SwiftUI body
    /// re-evaluations do not re-decode avatar images on every render.
    @MainActor
    static func cachedAvatar(data: Data) -> Image? {
        guard let image = DecodedImageCache.image(for: data) else { return nil }
        #if os(macOS)
        return Image(nsImage: image)
        #else
        return Image(uiImage: image)
        #endif
    }
}
