import SwiftUI
import WaddleKit

/// The emoji people in the call send, rising over the stage and fading
/// (web: `call-reaction-float`).
///
/// It reads the reactions itself, so one arriving or going redraws this
/// view and nothing else of the stage: no tile is laid out again and no
/// picture fetched, which matters beside live video.
struct CallReactionsOverlay: View {
    let reactions: CallReactions
    /// Our nick in the room of a room's call; `nil` in a call of two.
    var ownNick: String?

    var body: some View {
        CallReactionsField(bursts: reactions.bursts)
            // Each is said as it arrives, when nothing more pressing is being said.
            .onChange(of: reactions.bursts.last) { _, newest in
                guard let newest else { return }
                var text = AttributedString(newest.spoken(ownNick: ownNick))
                text.accessibilitySpeechAnnouncementPriority = .low
                AccessibilityNotification.Announcement(text).post()
            }
    }
}

/// The reactions themselves. Pure: what it shows is passed in.
struct CallReactionsField: View {
    let bursts: [CallReactionBurst]
    /// Off in a snapshot, which is a still: each emoji is drawn where it starts.
    var animates = true

    var body: some View {
        GeometryReader { stage in
            ForEach(bursts) { burst in
                CallReactionBubble(emoji: burst.emoji, rise: stage.size.height * Metrics.rise, animates: animates)
                    .position(
                        x: stage.size.width * (Metrics.inset + burst.lane * (1 - 2 * Metrics.inset)),
                        y: stage.size.height * Metrics.start
                    )
            }
        }
        .allowsHitTesting(false)
        // Said as each arrives; the picture of it is decoration.
        .accessibilityHidden(true)
    }

    private enum Metrics {
        /// Web: between 14% and 86% of the width, starting 16% above the bottom.
        static let inset = 0.14
        static let start = 0.84
        static let rise = 0.4
    }
}

/// One emoji: it appears, rises and is gone in the time the session shows it for.
private struct CallReactionBubble: View {
    let emoji: String
    let rise: CGFloat
    let animates: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isLeaving = false
    @ScaledMetric(relativeTo: .largeTitle) private var size: CGFloat = 34

    var body: some View {
        Text(emoji)
            .font(.system(size: size))
            .lineLimit(1)
            .fixedSize()
            // The fade has its own curve, slow and then quick, so the emoji
            // is seen most of the way; nothing outside this line is touched by it.
            .opacity(isLeaving ? 0 : 1)
            .animation(.easeIn(duration: CallReactionLedger.lifetime), value: isLeaving)
            .scaleEffect(reduceMotion ? 1 : (isLeaving ? 1.18 : 0.82))
            // With Reduce Motion it stays where it is and only fades.
            .offset(y: reduceMotion || !isLeaving ? 0 : -rise)
            .onAppear {
                guard animates else { return }
                // Rises at once and slows.
                withAnimation(.easeOut(duration: CallReactionLedger.lifetime)) { isLeaving = true }
            }
    }
}
