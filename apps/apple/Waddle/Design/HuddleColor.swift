import SwiftUI

#if os(macOS)
import AppKit
#else
import UIKit
#endif

extension Color {
    /// A Polar Huddle token colour: `night` in a dark appearance,
    /// `daylight` in a light one. Both are `0xAARRGGBB` in sRGB.
    ///
    /// The colour resolves when it is drawn, so it follows the system
    /// appearance, the in-app appearance preference and any
    /// `colorScheme` override on a view.
    init(night: UInt32, daylight: UInt32) {
        #if os(macOS)
        self.init(nsColor: NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            return NSColor(argb: isDark ? night : daylight)
        })
        #else
        self.init(uiColor: UIColor { traits in
            UIColor(argb: traits.userInterfaceStyle == .dark ? night : daylight)
        })
        #endif
    }
}

private func components(argb: UInt32) -> (red: CGFloat, green: CGFloat, blue: CGFloat, alpha: CGFloat) {
    let channel = { (shift: UInt32) in CGFloat((argb >> shift) & 0xFF) / 255 }
    return (channel(16), channel(8), channel(0), channel(24))
}

#if os(macOS)
private extension NSColor {
    convenience init(argb: UInt32) {
        let c = components(argb: argb)
        self.init(srgbRed: c.red, green: c.green, blue: c.blue, alpha: c.alpha)
    }
}
#else
private extension UIColor {
    convenience init(argb: UInt32) {
        let c = components(argb: argb)
        self.init(red: c.red, green: c.green, blue: c.blue, alpha: c.alpha)
    }
}
#endif
