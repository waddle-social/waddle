import SwiftUI
import WaddleKit

/// The emoji people in the call send, rising over the stage and fading
/// (web: `call-reaction-float`). It is laid over the tiles and takes no
/// part in them: a reaction redraws no tile and fetches no picture.
struct CallReactionsOverlay: View {
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
        // Said by the host as each arrives; the picture of it is decoration.
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
            .scaleEffect(reduceMotion ? 1 : (isLeaving ? 1.18 : 0.82))
            // With Reduce Motion it stays where it is and only fades.
            .offset(y: reduceMotion || !isLeaving ? 0 : -rise)
            .opacity(isLeaving ? 0 : 1)
            .onAppear {
                guard animates else { return }
                // Rises at once and slows; fades slowly and then quickly, so it is seen most of the way.
                withAnimation(.easeOut(duration: CallReactionLedger.lifetime)) { isLeaving = true }
            }
            .animation(.easeIn(duration: CallReactionLedger.lifetime), value: isLeaving)
    }
}
