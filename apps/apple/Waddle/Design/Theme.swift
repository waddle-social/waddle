import SwiftUI

/// Layout and type tokens. Colors come from the system palette plus the
/// brand accent (`AccentColor` asset) so the apps follow light/dark mode,
/// increased contrast and Dynamic Type without custom overrides.
enum Theme {
    enum Spacing {
        static let xxs: CGFloat = 2
        static let xs: CGFloat = 4
        static let s: CGFloat = 8
        static let m: CGFloat = 12
        static let l: CGFloat = 16
        static let xl: CGFloat = 24
    }

    enum Radius {
        static let small: CGFloat = 6
        static let medium: CGFloat = 10
        static let large: CGFloat = 16
        static let bubble: CGFloat = 18
    }

    enum Size {
        static let avatar: CGFloat = 36
        static let smallAvatar: CGFloat = 24
        static let rowAvatar: CGFloat = 28
        /// Max width for message media so it never spans a wide Mac window.
        static let mediaMaxWidth: CGFloat = 360
        /// Readable line length for the conversation column.
        static let readableWidth: CGFloat = 820
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

    static var secondaryBackground: Color {
        #if os(macOS)
        Color(nsColor: .controlBackgroundColor)
        #else
        Color(uiColor: .secondarySystemBackground)
        #endif
    }

    static var primaryBackground: Color {
        #if os(macOS)
        Color(nsColor: .windowBackgroundColor)
        #else
        Color(uiColor: .systemBackground)
        #endif
    }
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
