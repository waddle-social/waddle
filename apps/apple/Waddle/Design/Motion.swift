import SwiftUI

/// Polar Huddle motion. Springs are only for what the person caused.
///
/// With Reduce Motion each of these becomes a short ease, which is only a
/// fade if nothing in the view changes position or size. A view that
/// moves something (a sliding thumb, a scale) must read
/// `accessibilityReduceMotion` itself and swap the movement for an
/// opacity change, as the switch and segmented control do.
enum HuddleMotion {
    /// A control under the finger or pointer: 90ms.
    case press
    /// Something arriving where the person put it (a sent message, a
    /// dialog, a moved thumb): a snappy spring that settles.
    case settle
    /// A small celebration (a reaction, a new badge): a bouncy spring.
    case pop

    /// Scale of a pressed control.
    static let pressScale: CGFloat = 0.97

    func animation(reduceMotion: Bool) -> Animation {
        typealias Duration = WaddleTokens.Duration
        if reduceMotion {
            return .timingCurve(WaddleTokens.Easing.out, duration: Duration.fast)
        }
        switch self {
        case .press: return .timingCurve(WaddleTokens.Easing.out, duration: Duration.instant)
        case .settle: return .spring(duration: Duration.slow, bounce: 0.15)
        case .pop: return .spring(duration: Duration.slower, bounce: 0.35)
        }
    }
}

private struct HuddleAnimation<Value: Equatable>: ViewModifier {
    let motion: HuddleMotion
    let value: Value
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content.animation(motion.animation(reduceMotion: reduceMotion), value: value)
    }
}

extension View {
    /// Animates changes of `value` with a Polar Huddle motion; a short
    /// ease when Reduce Motion is on (see `HuddleMotion`).
    func huddleAnimation<Value: Equatable>(_ motion: HuddleMotion, value: Value) -> some View {
        modifier(HuddleAnimation(motion: motion, value: value))
    }
}
