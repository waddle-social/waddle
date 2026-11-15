import SwiftUI

/// Polar Huddle motion. Springs are only for what the person caused;
/// with Reduce Motion every one of these becomes a short fade.
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
    /// Animates changes of `value` with a Polar Huddle motion, as a fade
    /// when Reduce Motion is on.
    func huddleAnimation<Value: Equatable>(_ motion: HuddleMotion, value: Value) -> some View {
        modifier(HuddleAnimation(motion: motion, value: value))
    }
}
