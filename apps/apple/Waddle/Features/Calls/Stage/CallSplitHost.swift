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
    @Environment(CallStageModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @CallPlatform private var platform
    @ViewBuilder let content: Content

    var body: some View {
        let showsStage = CallSurface
            .placement(of: session.calls.state, open: navigation.selection, platform: platform, pane: model.pane)
            .showsStage
        // Before the first layout assume the common case, a pane with room beside.
        let arrangement = model.pane.flatMap(CallSurface.splitArrangement(in:)) ?? .beside(width: CallSurface.stageWidth)
        content
            .padding(showsStage ? inset(for: arrangement) : EdgeInsets())
            .overlay(alignment: .topTrailing) {
                if showsStage {
                    stage(arrangement)
                        .transition(reduceMotion ? .opacity : .move(edge: edge(of: arrangement)).combined(with: .opacity))
                }
            }
            .huddleAnimation(.settle, value: showsStage)
            .onGeometryChange(for: CGSize.self, of: \.size) { model.pane = $0 }
    }

    /// One stage whichever way it stands: moving it between beside and above,
    /// as when the inspector opens mid-call, must not rebuild its video
    /// views or say "Call connected" again.
    private func stage(_ arrangement: CallSurface.SplitArrangement) -> some View {
        let hair = WaddleTokens.BorderWidth.hair
        let (width, height): (CGFloat?, CGFloat?) = switch arrangement {
        case .beside(let width): (width, nil)
        case .above(let height): (nil, height)
        }
        return CallStageHost()
            .frame(width: width, height: height)
            // The edge it shares with the conversation.
            .overlay(alignment: width == nil ? .bottom : .leading) {
                Theme.Colors.borderSoft.frame(width: width == nil ? nil : hair, height: width == nil ? hair : nil)
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
