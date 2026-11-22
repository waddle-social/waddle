import SwiftUI

/// Sizes of the composer (web `wd-composer`): one row inside a panel,
/// 44pt touch targets on iOS and 32pt icon buttons for a pointer.
enum ComposerMetrics {
    /// Between the panel's edge and its buttons.
    static let inset = WaddleTokens.Spacing.s1Half
    static let cardRadius = WaddleTokens.Radius.panel
    /// A button's corner, concentric with the panel's.
    static let buttonRadius = cardRadius - inset
    #if os(iOS)
    static let button = WaddleTokens.ControlHeight.touch
    static let iconSize: CGFloat = 18
    #else
    static let button = WaddleTokens.ControlHeight.sm
    static let iconSize: CGFloat = 16
    #endif
}
