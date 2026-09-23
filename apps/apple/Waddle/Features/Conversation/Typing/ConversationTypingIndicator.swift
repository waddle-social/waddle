import SwiftUI
import WaddleKit

/// "bob is typing…" above the composer (XEP-0085).
struct ConversationTypingIndicator: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let conversation: ConversationID

    var body: some View {
        let sentence = TypingText.sentence(for: session.typing.names(in: conversation))
        ZStack(alignment: .leading) {
            if let sentence {
                HStack(spacing: Theme.Spacing.s - 2) {
                    TypingDots()
                    Text(sentence)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .padding(.horizontal, Theme.Spacing.l + 2)
                .transition(.opacity)
                .accessibilityElement(children: .combine)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: sentence)
    }
}

/// Three pulsing dots; static when Reduce Motion is on.
struct TypingDots: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(minimumInterval: 0.1, paused: reduceMotion)) { context in
            let phase = context.date.timeIntervalSinceReferenceDate
            HStack(spacing: 3) {
                ForEach(0..<3, id: \.self) { index in
                    Circle()
                        .frame(width: 5, height: 5)
                        .opacity(reduceMotion ? 0.6 : opacity(phase: phase, index: index))
                }
            }
        }
        .foregroundStyle(.secondary)
        .accessibilityHidden(true)
    }

    private func opacity(phase: TimeInterval, index: Int) -> Double {
        let wave = sin((phase * 2 * .pi / 1.2) - Double(index) * 0.8)
        return 0.35 + 0.65 * (wave + 1) / 2
    }
}
