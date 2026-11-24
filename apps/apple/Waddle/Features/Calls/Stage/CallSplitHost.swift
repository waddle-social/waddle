import SwiftUI
import WaddleKit

/// Shares the split view's detail pane between a conversation and the stage
/// of its call. A split stage stands beside the conversation while that
/// leaves it room to read, and above it otherwise; the edge they share can
/// be dragged. An expanded or full-screen stage takes the whole pane
/// (`CallSurface.stageArrangement`). With no call in session for the open
/// conversation it is just `content`.
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
        let state = session.calls.state
        let showsStage = model.surface(of: state, open: navigation.selection, platform: platform).showsStage
        let size = state.sid.map { model.stage(of: $0).size } ?? .defaultSize
        let conversation = navigation.selection
        let arrangement = model.pane.flatMap {
            CallSurface.stageArrangement(for: size, in: $0, splitPercent: conversation.flatMap(model.splitPercent(for:)))
        }
            // Before the first layout assume the common case, a pane with room beside.
            ?? (size == .split ? .beside(width: CallSurface.stageWidth) : .filling)
        let covers = showsStage && arrangement == .filling
        content
            .padding(showsStage ? inset(for: arrangement) : EdgeInsets())
            // Behind a stage that fills the pane the conversation is out of reach, not gone.
            .allowsHitTesting(!covers)
            .accessibilityHidden(covers)
            .overlay(alignment: .topTrailing) {
                if showsStage {
                    stage(arrangement, of: conversation)
                        .transition(reduceMotion ? .opacity : .move(edge: edge(of: arrangement)).combined(with: .opacity))
                }
            }
            .coordinateSpace(.named(Self.paneSpace))
            .huddleAnimation(.settle, value: showsStage)
            // A step in size settles; a drag follows the pointer.
            .huddleAnimation(.settle, value: size)
            .onGeometryChange(for: CGSize.self, of: \.size) { model.pane = $0 }
    }

    private static var paneSpace: String { "callPane" }

    /// One stage however it stands: moving it between beside, above and
    /// filling, as when the inspector opens mid-call or the call is
    /// expanded, must not rebuild its video views or say "Call connected" again.
    private func stage(_ arrangement: CallSurface.SplitArrangement, of conversation: ConversationID?) -> some View {
        let (width, height): (CGFloat?, CGFloat?) = switch arrangement {
        case .beside(let width): (width, nil)
        case .above(let height): (nil, height)
        case .filling: (nil, nil)
        }
        return CallStageHost()
            .frame(width: width, height: height)
            // The edge it shares with the conversation.
            .overlay(alignment: width == nil ? .bottom : .leading) {
                if let conversation, let pane = model.pane, arrangement != .filling {
                    handle(arrangement, in: pane, of: conversation)
                }
            }
    }

    private func handle(
        _ arrangement: CallSurface.SplitArrangement, in pane: CGSize, of conversation: ConversationID
    ) -> some View {
        let (axis, extent, paneExtent): (Axis, CGFloat, CGFloat) = switch arrangement {
        case .beside(let width): (.vertical, width, pane.width)
        case .above(let height): (.horizontal, height, pane.height)
        case .filling: (.horizontal, pane.height, pane.height)
        }
        return CallSplitHandle(
            axis: axis,
            percent: CallSurface.splitPercent(stage: extent, of: paneExtent),
            paneSpace: Self.paneSpace
        ) { location in
            // The stage is at the trailing edge when beside, at the top when above.
            let stage = axis == .vertical ? pane.width - location.x : location.y
            model.setSplitPercent(CallSurface.splitPercent(stage: stage, of: paneExtent), for: conversation)
        } step: { percent in
            model.setSplitPercent(percent, for: conversation)
        }
        // The line itself stays on the stage's first point; the reach straddles it.
        .offset(
            x: axis == .vertical ? -CallSplitHandle.overhang : 0, y: axis == .vertical ? 0 : CallSplitHandle.overhang
        )
    }

    private func inset(for arrangement: CallSurface.SplitArrangement) -> EdgeInsets {
        switch arrangement {
        case .beside(let width): EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: width)
        case .above(let height): EdgeInsets(top: height, leading: 0, bottom: 0, trailing: 0)
        case .filling: EdgeInsets()
        }
    }

    private func edge(of arrangement: CallSurface.SplitArrangement) -> Edge {
        switch arrangement {
        case .beside: .trailing
        case .above, .filling: .top
        }
    }
}

/// The edge a split stage shares with its conversation (web
/// `SplitDragHandle`): a hairline that can be dragged, with a grip that
/// shows under the pointer. VoiceOver adjusts it in steps.
struct CallSplitHandle: View {
    /// The way the line runs: vertical between a stage and the conversation beside it.
    let axis: Axis
    /// The stage's share of the pane now.
    let percent: Double
    /// The coordinate space of the pane the two share.
    let paneSpace: String
    /// Called with the pointer's place in the pane while it drags.
    let drag: @MainActor (CGPoint) -> Void
    /// Called with the share VoiceOver stepped to.
    let step: @MainActor (Double) -> Void
    @State private var isHovered = false
    @GestureState private var isDragging = false

    var body: some View {
        let isVertical = axis == .vertical
        let isActive = isHovered || isDragging
        Theme.Colors.borderSoft
            .frame(width: isVertical ? Metrics.hair : nil, height: isVertical ? nil : Metrics.hair)
            .overlay {
                Capsule()
                    .fill(isDragging ? Theme.Colors.action : Theme.Colors.fgSoft)
                    .frame(width: isVertical ? Metrics.grip : Metrics.gripLength, height: isVertical ? Metrics.gripLength : Metrics.grip)
                    .opacity(isActive ? 1 : 0)
            }
            .frame(width: isVertical ? Metrics.reach : nil, height: isVertical ? nil : Metrics.reach)
            .contentShape(Rectangle())
            .onHover { hovering in
                isHovered = hovering
                #if os(macOS)
                if hovering {
                    (isVertical ? NSCursor.resizeLeftRight : NSCursor.resizeUpDown).push()
                } else {
                    NSCursor.pop()
                }
                #endif
            }
            .gesture(
                DragGesture(minimumDistance: 1, coordinateSpace: .named(paneSpace))
                    .updating($isDragging) { _, dragging, _ in dragging = true }
                    .onChanged { drag($0.location) }
            )
            .onDisappear {
                #if os(macOS)
                if isHovered { NSCursor.pop() }
                #endif
            }
            .huddleAnimation(.press, value: isActive)
            .accessibilityElement()
            .accessibilityLabel(Text("Resize the call and the conversation"))
            .accessibilityValue(Text("Call takes \(Int(percent.rounded())) percent"))
            .accessibilityAdjustableAction { direction in
                switch direction {
                case .increment: step(percent + Metrics.step)
                case .decrement: step(percent - Metrics.step)
                @unknown default: break
                }
            }
            .help("Drag to resize")
    }

    /// How far the reach extends past the line on each side.
    static var overhang: CGFloat { (Metrics.reach - Metrics.hair) / 2 }

    private enum Metrics {
        static let hair = WaddleTokens.BorderWidth.hair
        /// The web handle's height: wide enough to catch without hunting.
        static let reach: CGFloat = 10
        static let grip: CGFloat = 4
        static let gripLength: CGFloat = 36
        static let step = 5.0
    }
}
