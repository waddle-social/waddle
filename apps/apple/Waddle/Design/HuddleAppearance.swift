import SwiftUI

#if os(iOS)
import UIKit
#endif

/// Polar Huddle on the system chrome SwiftUI cannot style: navigation bar
/// titles. Applied once at launch.
enum HuddleAppearance {
    @MainActor
    static func apply() {
        #if os(iOS)
        let navigationBar = UINavigationBar.appearance()
        navigationBar.largeTitleTextAttributes = [
            .font: scaled(.display, size: 32, weight: 800, relativeTo: .largeTitle),
        ]
        navigationBar.titleTextAttributes = [
            .font: scaled(.display, size: 17, weight: 700, relativeTo: .headline),
        ]
        #endif
    }

    #if os(iOS)
    private static func scaled(
        _ role: HuddleFont,
        size: CGFloat,
        weight: CGFloat,
        relativeTo style: UIFont.TextStyle
    ) -> UIFont {
        let font = HuddleFonts.font(role, size: size, weight: weight) as UIFont
        return UIFontMetrics(forTextStyle: style).scaledFont(for: font)
    }
    #endif
}
