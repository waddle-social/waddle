import CoreText
import SwiftUI

/// The bundled Polar Huddle typefaces: Bricolage Grotesque for titles,
/// Atkinson Hyperlegible Next for everything you read, Atkinson
/// Hyperlegible Mono for code. All three are variable fonts, so a weight
/// is a point on the `wght` axis and not a separate file.
enum HuddleFonts {
    private static let files = ["BricolageGrotesque", "AtkinsonHyperlegibleNext", "AtkinsonHyperlegibleMono"]

    /// Registers the bundled fonts for this process. Safe to call more
    /// than once; `font(_:size:weight:)` calls it for you.
    static func register() {
        _ = registered
    }

    private static let registered: Bool = {
        for file in files {
            guard let url = Bundle.main.url(forResource: file, withExtension: "ttf") else {
                assertionFailure("Missing bundled font \(file).ttf")
                continue
            }
            // Already-registered is the only expected failure (a second
            // process-wide registration), and it leaves the font usable.
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
        return true
    }()

    /// A face of `role` at `size` points and `weight` on the `wght` axis.
    static func font(_ role: HuddleFont, size: CGFloat, weight: CGFloat) -> CTFont {
        register()
        var variation: [NSNumber: CGFloat] = [Axis.weight: weight]
        if role.hasOpticalSize {
            // The web sets optical size to the font size (`font-optical-sizing:
            // auto`); the file's default is its 96pt master.
            variation[Axis.opticalSize] = min(max(size, 12), 96)
        }
        let attributes: [CFString: Any] = [
            kCTFontFamilyNameAttribute: role.familyName,
            kCTFontVariationAttribute: variation,
        ]
        let descriptor = CTFontDescriptorCreateWithAttributes(attributes as CFDictionary)
        return CTFontCreateWithFontDescriptor(descriptor, size, nil)
    }

    /// A SwiftUI font at a fixed size, for text sized from its container
    /// (avatar initials) instead of from a text style.
    static func swiftUIFont(_ role: HuddleFont, size: CGFloat, weight: CGFloat) -> Font {
        Font(font(role, size: size, weight: weight))
    }

    /// OpenType variation axis tags as Core Text identifiers.
    private enum Axis {
        static let weight = NSNumber(value: 0x7767_6874) // 'wght'
        static let opticalSize = NSNumber(value: 0x6F70_737A) // 'opsz'
    }
}

extension HuddleFont {
    var familyName: String {
        switch self {
        case .display: return "Bricolage Grotesque"
        case .body: return "Atkinson Hyperlegible Next"
        case .mono: return "Atkinson Hyperlegible Mono"
        }
    }

    fileprivate var hasOpticalSize: Bool { self == .display }
}

extension HuddleTextStyle {
    /// This style with another weight on the `wght` axis.
    func weight(_ weight: CGFloat) -> HuddleTextStyle {
        HuddleTextStyle(font: font, size: size, weight: weight, lineHeight: lineHeight, letterSpacing: letterSpacing)
    }

    /// This style at another size, keeping its line-height ratio.
    func sized(_ size: CGFloat) -> HuddleTextStyle {
        HuddleTextStyle(font: font, size: size, weight: weight, lineHeight: lineHeight, letterSpacing: letterSpacing)
    }

    /// The font, tracking and extra line spacing for this style scaled by
    /// `scale` (the Dynamic Type factor).
    func resolved(scale: CGFloat = 1) -> (font: Font, tracking: CGFloat, lineSpacing: CGFloat) {
        let pointSize = size * scale
        let face = HuddleFonts.font(font, size: pointSize, weight: weight)
        let natural = CTFontGetAscent(face) + CTFontGetDescent(face) + CTFontGetLeading(face)
        return (
            Font(face),
            letterSpacing * pointSize,
            max(0, lineHeight * pointSize - natural)
        )
    }
}

private struct HuddleTextModifier: ViewModifier {
    let style: HuddleTextStyle
    @ScaledMetric(relativeTo: .body) private var scale: CGFloat = 1

    func body(content: Content) -> some View {
        let resolved = style.resolved(scale: scale)
        content
            .font(resolved.font)
            .tracking(resolved.tracking)
            .lineSpacing(resolved.lineSpacing)
    }
}

extension View {
    /// Sets a Polar Huddle text style on the text in this view. Sizes
    /// follow Dynamic Type.
    func huddleText(_ style: HuddleTextStyle) -> some View {
        modifier(HuddleTextModifier(style: style))
    }
}
