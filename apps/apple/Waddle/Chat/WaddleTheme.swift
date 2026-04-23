import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

enum WaddleTheme {
    // MARK: - Backgrounds
#if os(macOS)
    static let chatBackground = Color(nsColor: .textBackgroundColor)
    static let sidebarBackground = Color(nsColor: .controlBackgroundColor)
    static let surfaceRaised = Color(nsColor: .controlBackgroundColor)
    static let surfaceHover = Color(nsColor: .underPageBackgroundColor)
    static let composerBackground = Color(nsColor: .textBackgroundColor)
#else
    static let chatBackground = Color(uiColor: .secondarySystemBackground)
    static let sidebarBackground = Color(uiColor: .systemGroupedBackground)
    static let surfaceRaised = Color(uiColor: .tertiarySystemBackground)
    static let surfaceHover = Color(uiColor: .quaternarySystemFill)
    static let composerBackground = Color(uiColor: .secondarySystemBackground)
#endif

    // MARK: - Text
    static let textPrimary = Color.primary
    static let textSecondary = Color.secondary
    static let textMuted = Color.secondary.opacity(0.78)
    static let textLink = Color.accentColor

    // MARK: - Accent
    static let accent = Color.accentColor
    static let mentionHighlight = Color.accentColor.opacity(0.14)
    static let unreadBadge = Color(red: 0.82, green: 0.29, blue: 0.29)

    // MARK: - Presence
    static let presenceOnline = Color(red: 0.18, green: 0.80, blue: 0.44)
    static let presenceAway = Color(red: 0.95, green: 0.77, blue: 0.06)
    static let presenceDnd = Color(red: 0.86, green: 0.37, blue: 0.35)
    static let presenceOffline = Color(white: 0.40)

    // MARK: - Messages
    static let ownMessageBubble = Color.accentColor.opacity(0.12)
    static let messageHover = Color.primary.opacity(0.05)

    // MARK: - Dividers & Borders
#if os(macOS)
    static let divider = Color(nsColor: .separatorColor).opacity(0.4)
#else
    static let divider = Color(uiColor: .separator).opacity(0.4)
#endif
    static let channelSelected = Color.accentColor.opacity(0.12)

    // MARK: - Spacing
    static let messageAvatarSize: CGFloat = 36
    static let messageSpacingY: CGFloat = 2
    static let messageClusterSpacingY: CGFloat = 16
    static let composerHeight: CGFloat = 44
    static let sidebarWidth: CGFloat = 260

    // MARK: - Typography
    static let senderFont = Font.subheadline.weight(.semibold)
    static let timestampFont = Font.caption2
    static let bodyFont = Font.subheadline
    static let channelFont = Font.subheadline

    // MARK: - Avatar Colors (consistent hue per username)
    static func avatarColor(for name: String) -> Color {
        var hash: UInt32 = 5381
        for char in name.unicodeScalars {
            hash = ((hash &<< 5) &+ hash) &+ char.value
        }
        let hue = Double(hash % 360) / 360.0
        return Color(hue: hue, saturation: 0.55, brightness: 0.75)
    }
}
