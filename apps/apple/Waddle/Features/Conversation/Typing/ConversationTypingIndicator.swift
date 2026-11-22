import SwiftUI
import WaddleKit

/// "bob is typing…" above the composer (XEP-0085): the waddling dots,
/// then the sentence (web `wd-typing`).
struct ConversationTypingIndicator: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let conversation: ConversationID

    var body: some View {
        let sentence = TypingText.sentence(for: session.typing.names(in: conversation))
        ZStack(alignment: .leading) {
            if let sentence {
                HStack(spacing: Theme.Spacing.s) {
                    TypingDots()
                    Text(sentence)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                }
                .padding(.horizontal, Theme.Spacing.xl)
                .transition(.opacity)
                .accessibilityElement(children: .combine)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: sentence)
    }
}

/// Three dots that rock side to side like a penguin walking (web
/// `wd-waddle`), in a bubble with its tail corner at the bottom left.
/// Still when Reduce Motion is on.
struct TypingDots: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static let period: TimeInterval = 1.2
    private static let stagger: TimeInterval = 0.15

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 30, paused: reduceMotion)) { context in
            let time = context.date.timeIntervalSinceReferenceDate
            HStack(spacing: Theme.Spacing.xs) {
                ForEach(0..<3, id: \.self) { index in
                    let step = reduceMotion ? 0 : Self.step(time - Double(index) * Self.stagger)
                    Circle()
                        .fill(Theme.Colors.fgMuted)
                        .frame(width: 7, height: 7)
                        .rotationEffect(.degrees(-12 + 24 * step))
                        .offset(x: -1.5 + 3 * step, y: -2 * step)
                }
            }
        }
        .padding(.horizontal, WaddleTokens.Spacing.s2Half)
        .padding(.vertical, WaddleTokens.Spacing.s2)
        .background(
            UnevenRoundedRectangle(
                topLeadingRadius: Theme.Radius.bubble,
                bottomLeadingRadius: Theme.Radius.small,
                bottomTrailingRadius: Theme.Radius.bubble,
                topTrailingRadius: Theme.Radius.bubble,
                style: .continuous
            )
            .fill(Theme.Colors.surface2)
        )
        .accessibilityHidden(true)
    }

    /// 0 at either end of the period, 1 in its middle, eased in and out.
    private static func step(_ time: TimeInterval) -> Double {
        let phase = time.truncatingRemainder(dividingBy: period) / period
        return (1 - cos(phase * 2 * .pi)) / 2
    }
}
