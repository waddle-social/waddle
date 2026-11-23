import SwiftUI
import WaddleKit

/// Shares the split view's detail pane between a conversation and the stage
/// of its call: the stage stands beside the conversation while that leaves
/// it room to read, and above it otherwise (`CallSurface.splitArrangement`).
/// With no call in session for the open conversation it is just `content`.
///
/// The conversation keeps its place in the view tree whether or not the
/// stage is there, so a call starting or ending never rebuilds the timeline.
struct CallSplitHost<Content: View>: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ViewBuilder let content: Content
    /// The pane's size, once it has been laid out.
    @State private var pane: CGSize?

    var body: some View {
        let showsStage = CallSurface
            .placement(of: session.calls.state, open: navigation.selection, platform: .regularWidth)
            .showsStage
        // Before the first layout assume the common case, a pane with room beside.
        let arrangement = pane.map(CallSurface.splitArrangement(in:)) ?? .beside(width: CallSurface.stageWidth)
        content
            .padding(showsStage ? inset(for: arrangement) : EdgeInsets())
            .overlay(alignment: .topTrailing) {
                if showsStage {
                    stage(arrangement)
                        .transition(reduceMotion ? .opacity : .move(edge: edge(of: arrangement)).combined(with: .opacity))
                }
            }
            .huddleAnimation(.settle, value: showsStage)
            .onGeometryChange(for: CGSize.self, of: \.size) { pane = $0 }
    }

    @ViewBuilder
    private func stage(_ arrangement: CallSurface.SplitArrangement) -> some View {
        let hairline = Theme.Colors.borderSoft
        switch arrangement {
        case .beside(let width):
            CallStageHost()
                .frame(width: width)
                .overlay(alignment: .leading) { hairline.frame(width: WaddleTokens.BorderWidth.hair) }
        case .above(let height):
            CallStageHost()
                .frame(height: height)
                .overlay(alignment: .bottom) { hairline.frame(height: WaddleTokens.BorderWidth.hair) }
        }
    }

    private func inset(for arrangement: CallSurface.SplitArrangement) -> EdgeInsets {
        switch arrangement {
        case .beside(let width): EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: width)
        case .above(let height): EdgeInsets(top: height, leading: 0, bottom: 0, trailing: 0)
        }
    }

    private func edge(of arrangement: CallSurface.SplitArrangement) -> Edge {
        switch arrangement {
        case .beside: .trailing
        case .above: .top
        }
    }
}
