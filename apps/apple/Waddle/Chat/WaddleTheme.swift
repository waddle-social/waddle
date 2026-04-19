import SwiftUI

enum WaddleTheme {
    // MARK: - Backgrounds
    static let chatBackground = Color(red: 0.07, green: 0.07, blue: 0.11)
    static let sidebarBackground = Color(red: 0.05, green: 0.05, blue: 0.09)
    static let surfaceRaised = Color(red: 0.11, green: 0.11, blue: 0.16)
    static let surfaceHover = Color(red: 0.14, green: 0.14, blue: 0.20)
    static let composerBackground = Color(red: 0.10, green: 0.10, blue: 0.15)

    // MARK: - Text
    static let textPrimary = Color.white
    static let textSecondary = Color(white: 0.62)
    static let textMuted = Color(white: 0.40)
    static let textLink = Color(red: 0.35, green: 0.55, blue: 1.0)

    // MARK: - Accent
    static let accent = Color(red: 0.37, green: 0.36, blue: 0.90)
    static let mentionHighlight = Color(red: 0.35, green: 0.55, blue: 1.0).opacity(0.15)
    static let unreadBadge = Color.red

    // MARK: - Presence
    static let presenceOnline = Color(red: 0.18, green: 0.80, blue: 0.44)
    static let presenceAway = Color(red: 0.95, green: 0.77, blue: 0.06)
    static let presenceDnd = Color.red
    static let presenceOffline = Color(white: 0.40)

    // MARK: - Messages
    static let ownMessageBubble = Color(red: 0.37, green: 0.36, blue: 0.90).opacity(0.12)
    static let messageHover = Color.white.opacity(0.03)

    // MARK: - Dividers & Borders
    static let divider = Color.white.opacity(0.06)
    static let channelSelected = Color.white.opacity(0.08)

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
}
