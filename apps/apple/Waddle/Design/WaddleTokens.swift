// GENERATED — do not edit.
// Source: design/src/preset.ts (Polar Huddle)
// Regenerate: bun scripts/generate-design-tokens.mjs
import SwiftUI

/// Polar Huddle design tokens, shared with the web and Android clients.
enum WaddleTokens {
    /// Semantic colours. Each resolves to its Night value in a dark
    /// appearance and its Daylight value in a light one.
    enum Colors {
        static let bg = Color(night: 0xFF060F19, daylight: 0xFFF8FAFD)
        static let bgRail = Color(night: 0xFF0A131E, daylight: 0xFFFFFFFF)
        static let surface2 = Color(night: 0xFF1A242F, daylight: 0xFFEDF0F4)
        static let surface3 = Color(night: 0xFF232E39, daylight: 0xFFFFFFFF)
        static let surface = Color(night: 0xFF111A25, daylight: 0xFFFFFFFF)
        static let surfaceCode = Color(night: 0xFF020912, daylight: 0xFFEDF0F4)
        static let borderSoft = Color(night: 0xFF28333F, daylight: 0xFFDBE0E6)
        static let border = Color(night: 0xFF3B4653, daylight: 0xFFC8CED6)
        static let borderStrong = Color(night: 0xFF9AA2AD, daylight: 0xFF737D88)
        static let fg = Color(night: 0xFFEDF0F4, daylight: 0xFF060F19)
        static let fgMuted = Color(night: 0xFFC1C8D0, daylight: 0xFF38424E)
        static let fgSoft = Color(night: 0xFF9AA2AD, daylight: 0xFF525C67)
        static let action = Color(night: 0xFFFFC139, daylight: 0xFF0F4376)
        static let actionHover = Color(night: 0xFFFFD27D, daylight: 0xFF062C50)
        static let actionFg = Color(night: 0xFF060F19, daylight: 0xFFFFFFFF)
        static let actionText = Color(night: 0xFFFFD27D, daylight: 0xFF6B4C00)
        static let actionSubtle = Color(night: 0xFF2B1C00, daylight: 0xFFFFEED0)
        static let live = Color(night: 0xFF4ED7DF, daylight: 0xFF007176)
        static let liveHover = Color(night: 0xFF85E5EA, daylight: 0xFF005256)
        static let liveFg = Color(night: 0xFF060F19, daylight: 0xFFFFFFFF)
        static let liveText = Color(night: 0xFF85E5EA, daylight: 0xFF005256)
        static let liveSubtle = Color(night: 0xFF002021, daylight: 0xFFEBFAFB)
        static let danger = Color(night: 0xFFD33944, daylight: 0xFFD33944)
        static let dangerHover = Color(night: 0xFFAA1D2C, daylight: 0xFFAA1D2C)
        static let dangerFg = Color(night: 0xFFFFFFFF, daylight: 0xFFFFFFFF)
        static let dangerText = Color(night: 0xFFFFAFAC, daylight: 0xFFAA1D2C)
        static let dangerSubtle = Color(night: 0xFF360307, daylight: 0xFFFFF4F3)
        static let warning = Color(night: 0xFFFC924B, daylight: 0xFFE77412)
        static let warningText = Color(night: 0xFFFFB486, daylight: 0xFF924500)
        static let warningSubtle = Color(night: 0xFF2D1000, daylight: 0xFFFFF4EE)
        static let success = Color(night: 0xFF67C986, daylight: 0xFF0A924B)
        static let successText = Color(night: 0xFF92DBA6, daylight: 0xFF017137)
        static let successSubtle = Color(night: 0xFF00210B, daylight: 0xFFEEFBF1)
        static let info = Color(night: 0xFF78B5F9, daylight: 0xFF397DC4)
        static let infoText = Color(night: 0xFF9ECCFF, daylight: 0xFF205F9F)
        static let infoSubtle = Color(night: 0xFF041A32, daylight: 0xFFF1F8FF)
        static let focus = Color(night: 0xFF9ECCFF, daylight: 0xFF397DC4)
        static let presenceAvailable = Color(night: 0xFF67C986, daylight: 0xFF0A924B)
        static let presenceAway = Color(night: 0xFFFFC139, daylight: 0xFFBB8800)
        static let presenceDnd = Color(night: 0xFFFF8987, daylight: 0xFFD33944)
        static let presenceOffline = Color(night: 0xFF9AA2AD, daylight: 0xFF737D88)
        static let gold = Color(night: 0xFFFFC139, daylight: 0xFFFFC139)
        static let goldFg = Color(night: 0xFF060F19, daylight: 0xFF060F19)
        static let goldText = Color(night: 0xFFFFD27D, daylight: 0xFF6B4C00)
        static let goldSubtle = Color(night: 0xFF2B1C00, daylight: 0xFFFFEED0)
        static let halo = Color(night: 0xFFC4DFFF, daylight: 0xFF205F9F)
        static let mist = Color(night: 0xFFC1C8D0, daylight: 0xFF38424E)
        static let sea = Color(night: 0xFF041A32, daylight: 0xFFF1F8FF)
        static let seaDeep = Color(night: 0xFF030F1F, daylight: 0xFFE0EFFF)
        static let seaShallow = Color(night: 0xFF062C50, daylight: 0xFFC4DFFF)
        static let avatarFg = Color(night: 0xFF060F19, daylight: 0xFF060F19)
        static let scrim = Color(night: 0xB8020912, daylight: 0x80060F19)
        static let selection = Color(night: 0x5278B5F9, daylight: 0x669ECCFF)
        static let belly = Color(night: 0xFFFFFFFF, daylight: 0xFFFFFFFF)
        static let ink = Color(night: 0xFF14181E, daylight: 0xFF14181E)
        static let beak = Color(night: 0xFFFF8A3D, daylight: 0xFFFF8A3D)

        /// Avatar fallback tints (web `avatar.N`), one picked per name.
        static let avatarTints: [Color] = [
            Color(night: 0xFFFFE0A9, daylight: 0xFFFFE0A9),
            Color(night: 0xFFC4DFFF, daylight: 0xFFC4DFFF),
            Color(night: 0xFFDBE0E6, daylight: 0xFFDBE0E6),
            Color(night: 0xFFB4EEF1, daylight: 0xFFB4EEF1),
            Color(night: 0xFFFFCFCC, daylight: 0xFFFFCFCC),
            Color(night: 0xFFBCEAC7, daylight: 0xFFBCEAC7),
        ]
    }

    /// 4pt grid (web `spacing.N`). Half steps are for inside controls only.
    enum Spacing {
        static let s0Half: CGFloat = 2
        static let s1: CGFloat = 4
        static let s1Half: CGFloat = 6
        static let s2: CGFloat = 8
        static let s2Half: CGFloat = 10
        static let s3: CGFloat = 12
        static let s4: CGFloat = 16
        static let s5: CGFloat = 20
        static let s6: CGFloat = 24
        static let s8: CGFloat = 32
        static let s10: CGFloat = 40
        static let s12: CGFloat = 48
        static let s16: CGFloat = 64
    }

    enum Radius {
        static let xs: CGFloat = 4
        static let sm: CGFloat = 8
        static let tag: CGFloat = 6
        static let control: CGFloat = 12
        static let panel: CGFloat = 16
        static let card: CGFloat = 20
        static let bubble: CGFloat = 20
        static let sheet: CGFloat = 28
        static let pill: CGFloat = 999
    }

    enum BorderWidth {
        static let hair: CGFloat = 1
        static let strong: CGFloat = 2
        static let focus: CGFloat = 3
    }

    /// Control heights (comfortable density; touch is the 44pt target).
    enum ControlHeight {
        static let xs: CGFloat = 28
        static let sm: CGFloat = 32
        static let md: CGFloat = 36
        static let lg: CGFloat = 40
        static let touch: CGFloat = 44
    }

    enum FontSize {
        static let xxs: CGFloat = 12
        static let xs: CGFloat = 13
        static let sm: CGFloat = 14
        static let md: CGFloat = 15
        static let base: CGFloat = 16
        static let lg: CGFloat = 18
        static let xl: CGFloat = 20
        static let xxl: CGFloat = 24
        static let xxxl: CGFloat = 30
        static let xxxxl: CGFloat = 40
        static let xxxxxl: CGFloat = 60
    }

    /// Web `textStyles`.
    enum TextStyles {
        static let hero = HuddleTextStyle(font: .display, size: 60, weight: 800, lineHeight: 1, letterSpacing: -0.03)
        static let display = HuddleTextStyle(font: .display, size: 40, weight: 800, lineHeight: 1.1, letterSpacing: -0.025)
        static let headline = HuddleTextStyle(font: .display, size: 30, weight: 750, lineHeight: 1.2, letterSpacing: -0.02)
        static let title = HuddleTextStyle(font: .display, size: 20, weight: 700, lineHeight: 1.3, letterSpacing: -0.01)
        static let subtitle = HuddleTextStyle(font: .display, size: 18, weight: 700, lineHeight: 1.333, letterSpacing: -0.01)
        static let body = HuddleTextStyle(font: .body, size: 15, weight: 400, lineHeight: 1.467, letterSpacing: 0)
        static let message = HuddleTextStyle(font: .body, size: 16, weight: 400, lineHeight: 1.5, letterSpacing: 0)
        static let author = HuddleTextStyle(font: .body, size: 15, weight: 700, lineHeight: 1.333, letterSpacing: 0)
        static let timestamp = HuddleTextStyle(font: .body, size: 13, weight: 400, lineHeight: 1.385, letterSpacing: 0)
        static let channel = HuddleTextStyle(font: .display, size: 18, weight: 700, lineHeight: 1.333, letterSpacing: 0)
        static let sidebar = HuddleTextStyle(font: .body, size: 15, weight: 500, lineHeight: 1.333, letterSpacing: 0)
        static let sidebarSection = HuddleTextStyle(font: .body, size: 13, weight: 700, lineHeight: 1.385, letterSpacing: 0)
        static let badge = HuddleTextStyle(font: .body, size: 12, weight: 700, lineHeight: 1, letterSpacing: 0.01)
        static let tag = HuddleTextStyle(font: .body, size: 12, weight: 600, lineHeight: 1, letterSpacing: 0.01)
        static let button = HuddleTextStyle(font: .body, size: 14, weight: 600, lineHeight: 1, letterSpacing: 0)
        static let input = HuddleTextStyle(font: .body, size: 15, weight: 400, lineHeight: 1.467, letterSpacing: 0)
        static let label = HuddleTextStyle(font: .body, size: 14, weight: 600, lineHeight: 1.429, letterSpacing: 0)
        static let caption = HuddleTextStyle(font: .body, size: 13, weight: 400, lineHeight: 1.385, letterSpacing: 0.005)
        static let eyebrow = HuddleTextStyle(font: .body, size: 13, weight: 700, lineHeight: 1.385, letterSpacing: 0.01)
        static let code = HuddleTextStyle(font: .mono, size: 14, weight: 400, lineHeight: 1.429, letterSpacing: 0)
    }

    /// Durations in seconds.
    enum Duration {
        static let instant: TimeInterval = 0.09
        static let fast: TimeInterval = 0.15
        static let base: TimeInterval = 0.22
        static let slow: TimeInterval = 0.32
        static let slower: TimeInterval = 0.48
        static let ambient: TimeInterval = 2.4
    }

    enum Easing {
        static let out = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.2, y: 0.7), endControlPoint: UnitPoint(x: 0.2, y: 1))
        static let easeIn = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.4, y: 0), endControlPoint: UnitPoint(x: 1, y: 1))
        static let inOut = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.65, y: 0), endControlPoint: UnitPoint(x: 0.35, y: 1))
        static let spring = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.34, y: 1.56), endControlPoint: UnitPoint(x: 0.64, y: 1))
    }
}

/// Polar Huddle font roles (web `fonts` tokens).
enum HuddleFont: CaseIterable {
    case display
    case body
    case mono
}

/// A Polar Huddle text style. `lineHeight` is a multiple of `size`;
/// `letterSpacing` is in em.
struct HuddleTextStyle: Equatable {
    let font: HuddleFont
    let size: CGFloat
    let weight: CGFloat
    let lineHeight: CGFloat
    let letterSpacing: CGFloat
}
