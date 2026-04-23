import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

enum WaddleTheme {

    // MARK: - Internal adaptive-color helpers

#if os(macOS)
    /// Returns a SwiftUI `Color` that resolves to `light` in Aqua and `dark`
    /// in Dark Aqua appearances, matching how NSAppearance resolves at draw time.
    private static func adaptive(light: NSColor, dark: NSColor) -> Color {
        Color(NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light
        })
    }

    /// Convenience: RGB 0–255 → NSColor, optional alpha.
    private static func ns(
        _ r: CGFloat, _ g: CGFloat, _ b: CGFloat, a: CGFloat = 1
    ) -> NSColor {
        NSColor(red: r / 255, green: g / 255, blue: b / 255, alpha: a)
    }
#else
    private static func adaptive(light: UIColor, dark: UIColor) -> Color {
        Color(UIColor { $0.userInterfaceStyle == .dark ? dark : light })
    }

    private static func ui(
        _ r: CGFloat, _ g: CGFloat, _ b: CGFloat, a: CGFloat = 1
    ) -> UIColor {
        UIColor(red: r / 255, green: g / 255, blue: b / 255, alpha: a)
    }
#endif

    // MARK: - Backgrounds

#if os(macOS)
    /// Main chat canvas — deep ink slate (dark) / cool off-white (light).
    static let chatBackground = adaptive(
        light: ns(244, 245, 250),
        dark:  ns(13,  14,  21)
    )
    /// Channel list, member panel, and other side panels.
    static let sidebarBackground = adaptive(
        light: ns(233, 235, 245),
        dark:  ns(9,   10,  16)
    )
    /// Far-left workspace rail.
    static let railBackground = adaptive(
        light: ns(222, 225, 238),
        dark:  ns(7,   8,   13)
    )
    /// Cards, text inputs, and elevated surfaces.
    static let surfaceRaised = adaptive(
        light: ns(255, 255, 255),
        dark:  ns(21,  23,  32)
    )
    /// Tint applied to interactive surfaces on pointer hover.
    static let surfaceHover = adaptive(
        light: ns(0,   0,   0,   a: 0.04),
        dark:  ns(255, 255, 255, a: 0.05)
    )
    /// Composer text-entry background.
    static let composerBackground = adaptive(
        light: ns(255, 255, 255),
        dark:  ns(18,  20,  28)
    )
#else
    static let chatBackground = adaptive(
        light: ui(244, 245, 250),
        dark:  ui(13,  14,  21)
    )
    static let sidebarBackground = adaptive(
        light: ui(233, 235, 245),
        dark:  ui(9,   10,  16)
    )
    static let railBackground = adaptive(
        light: ui(222, 225, 238),
        dark:  ui(7,   8,   13)
    )
    static let surfaceRaised = adaptive(
        light: ui(255, 255, 255),
        dark:  ui(21,  23,  32)
    )
    static let surfaceHover = adaptive(
        light: ui(0,   0,   0,   a: 0.04),
        dark:  ui(255, 255, 255, a: 0.05)
    )
    static let composerBackground = adaptive(
        light: ui(255, 255, 255),
        dark:  ui(18,  20,  28)
    )
#endif

    // MARK: - Text

    static let textPrimary   = Color.primary
    static let textSecondary = Color.secondary
    static let textMuted     = Color.secondary.opacity(0.68)
    static let textLink      = Color.accentColor

    // MARK: - Accent

    static let accent           = Color.accentColor
    static let mentionHighlight = Color.accentColor.opacity(0.13)
    static let channelSelected  = Color.accentColor.opacity(0.13)

    // MARK: - Badges

    /// Vivid red-rose for unread counts — punchy, not brownish.
    static let unreadBadge = Color(red: 0.93, green: 0.22, blue: 0.37)

    // MARK: - Presence

    static let presenceOnline  = Color(red: 0.12, green: 0.88, blue: 0.48)
    static let presenceAway    = Color(red: 0.98, green: 0.76, blue: 0.10)
    static let presenceDnd     = Color(red: 0.96, green: 0.28, blue: 0.34)
    static let presenceOffline = Color(white: 0.40)

    // MARK: - Messages

    static let ownMessageBubble = Color.accentColor.opacity(0.10)
    static let messageHover     = Color.accentColor.opacity(0.045)

    // MARK: - Dividers & Borders

#if os(macOS)
    static let divider = adaptive(
        light: ns(0,   0,   0,   a: 0.08),
        dark:  ns(255, 255, 255, a: 0.07)
    )
#else
    static let divider = Color(UIColor { trait in
        trait.userInterfaceStyle == .dark
            ? UIColor(white: 1, alpha: 0.07)
            : UIColor(white: 0, alpha: 0.08)
    })
#endif

    // MARK: - Sizing

    /// Square-ish avatar tile in message rows.
    static let messageAvatarSize:      CGFloat = 36
    static let messageSpacingY:        CGFloat = 2
    static let messageClusterSpacingY: CGFloat = 16
    static let composerHeight:         CGFloat = 44
    static let sidebarWidth:           CGFloat = 260

    // MARK: - Typography

    /// Sender name in message rows — `.rounded` adds warmth without custom fonts.
    static let senderFont    = Font.system(size: 13,   weight: .semibold, design: .rounded)
    /// Timestamps and small metadata labels.
    static let timestampFont = Font.system(size: 11,   weight: .medium)
    /// Message body — standard design optimised for reading.
    static let bodyFont      = Font.system(size: 14.5)
    /// Channel and DM names in the sidebar rail.
    static let channelFont   = Font.system(size: 13.5, weight: .medium,  design: .rounded)

    // MARK: - Avatar Colors

    /// Derives a consistent, moderately vibrant hue from a display name.
    static func avatarColor(for name: String) -> Color {
        var hash: UInt32 = 5381
        for char in name.unicodeScalars {
            hash = ((hash &<< 5) &+ hash) &+ char.value
        }
        let hue = Double(hash % 360) / 360.0
        return Color(hue: hue, saturation: 0.60, brightness: 0.78)
    }
}
