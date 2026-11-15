import SwiftUI

/// The app's design vocabulary: Polar Huddle tokens (`WaddleTokens`,
/// generated from `design/src/preset.ts`) under the names views use, plus
/// the app's own layout sizes.
enum Theme {
    /// Semantic colours; each follows the Night or Daylight appearance.
    typealias Colors = WaddleTokens.Colors

    /// Text styles, applied with `.huddleText(_:)`.
    typealias Typography = WaddleTokens.TextStyles

    enum Spacing {
        static let xxs = WaddleTokens.Spacing.s0_5
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
        /// Max width for message media so it never spans a wide Mac window.
        static let mediaMaxWidth: CGFloat = 360
        /// Readable line length for the conversation column.
        static let readableWidth: CGFloat = 820
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

extension Image {
    /// Decodes image bytes on either platform.
    init?(data: Data) {
        #if os(macOS)
        guard let image = NSImage(data: data) else { return nil }
        self.init(nsImage: image)
        #else
        guard let image = UIImage(data: data) else { return nil }
        self.init(uiImage: image)
        #endif
    }
}
