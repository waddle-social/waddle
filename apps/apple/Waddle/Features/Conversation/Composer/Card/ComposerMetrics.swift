import SwiftUI

/// Sizes of the composer card's controls: 44pt touch targets on iOS,
/// compact pointer targets on Mac.
enum ComposerMetrics {
    #if os(iOS)
    static let touchTarget: CGFloat = 44
    static let iconSize: CGFloat = 18
    static let plusDiameter: CGFloat = 30
    static let sendWidth: CGFloat = 40
    static let sendHeight: CGFloat = 34
    static let sendSymbolSize: CGFloat = 16
    static let cardRadius: CGFloat = 20
    #else
    static let touchTarget: CGFloat = 28
    static let iconSize: CGFloat = 15
    static let plusDiameter: CGFloat = 22
    static let sendWidth: CGFloat = 34
    static let sendHeight: CGFloat = 26
    static let sendSymbolSize: CGFloat = 13
    static let cardRadius: CGFloat = Theme.Radius.large
    #endif
}
