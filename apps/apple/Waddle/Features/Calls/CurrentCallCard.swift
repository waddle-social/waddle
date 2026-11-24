import SwiftUI
import WaddleKit

/// The sidebar's card for a call in session whose conversation is not the
/// one open (web `CurrentCallPanel`): what the call is and how it is doing,
/// the way back to it, and the controls that cannot wait until you are there.
///
/// Pure: everything it shows is passed in.
struct CurrentCallCard: View {
    struct Actions {
        /// Opens the call's conversation, where its stage is.
        let returnToCall: @MainActor () -> Void
        /// As `returnToCall`, with the stage expanded.
        let expand: @MainActor () -> Void
        let toggleMicrophone: @MainActor () -> Void
        let toggleCamera: @MainActor () -> Void
        let hangUp: @MainActor () -> Void
    }

    /// The peer, or the room.
    let title: String
    /// `CallCardSummary.line`.
    let summary: String
    /// Media is up: the dot is live.
    var isConnected = true
    /// The devices can be switched: media has connected, even if it is reconnecting now.
    var hasMedia = true
    var isMicrophoneOn = false
    var isCameraOn = false
    let actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            HStack(alignment: .top, spacing: WaddleTokens.Spacing.s2Half) {
                Circle()
                    .fill(isConnected ? Theme.Colors.live : Theme.Colors.fgSoft)
                    .frame(width: Theme.Size.liveDot, height: Theme.Size.liveDot)
                    // Beside the title, wherever the summary wraps.
                    .padding(.top, Metrics.dotInset)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 0) {
                    Text(title)
                        .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.sm))
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(1)
                    // The whole line matters: at the sidebar's narrowest it takes a second row.
                    Text(summary)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.liveText)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .accessibilityElement(children: .combine)
                Spacer(minLength: 0)
            }
            HStack(spacing: WaddleTokens.Spacing.s1Half) {
                control(
                    isMicrophoneOn ? "Mute" : "Unmute", symbol: isMicrophoneOn ? "mic" : "mic.slash",
                    isOff: !isMicrophoneOn && hasMedia, action: actions.toggleMicrophone
                )
                .disabled(!hasMedia)
                control(
                    isCameraOn ? "Turn camera off" : "Turn camera on", symbol: isCameraOn ? "video" : "video.slash",
                    action: actions.toggleCamera
                )
                .disabled(!hasMedia)
                control("Expand call", symbol: "arrow.up.left.and.arrow.down.right", action: actions.expand)
                Spacer(minLength: 0)
                // Beside the controls, not the text: the sidebar is too narrow to share a line with the summary.
                Button("Return", action: actions.returnToCall)
                    .buttonStyle(.huddle(size: .small))
                    .fixedSize()
                    .accessibilityLabel(Text("Return to the call with \(title)"))
                    .help("Open the call's conversation")
                Button(action: actions.hangUp) {
                    glyph("phone.down", foreground: Theme.Colors.dangerFg, background: Theme.Colors.danger)
                }
                .buttonStyle(CurrentCallControlStyle())
                .accessibilityLabel(Text("Hang up"))
                .help("Hang up")
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(Text("Current call controls"))
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.vertical, WaddleTokens.Spacing.s2Half)
        .background(Metrics.shape.fill(Theme.Colors.liveSubtle))
        .overlay(Metrics.shape.strokeBorder(Theme.Colors.live, lineWidth: WaddleTokens.BorderWidth.hair))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Current call"))
    }

    private func control(
        _ title: String, symbol: String, isOff: Bool = false, action: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: action) {
            glyph(
                symbol, foreground: isOff ? Theme.Colors.dangerText : Theme.Colors.fg,
                background: isOff ? Theme.Colors.danger.opacity(0.18) : Theme.Colors.surface
            )
        }
        .buttonStyle(CurrentCallControlStyle())
        .accessibilityLabel(Text(title))
        .help(title)
    }

    private func glyph(_ symbol: String, foreground: Color, background: Color) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 14, weight: .medium))
            .foregroundStyle(foreground)
            .frame(width: Metrics.control, height: Metrics.control)
            .background(Circle().fill(background))
            .contentShape(Circle())
    }

    private enum Metrics {
        static let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        static let dotInset: CGFloat = 5
        /// The web's small icon button under a pointer; a finger gets the touch height.
        #if os(macOS)
        static let control: CGFloat = 32
        #else
        static let control = WaddleTokens.ControlHeight.touch
        #endif
    }
}

private struct CurrentCallControlStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .overlay(Circle().fill(configuration.isPressed ? Theme.State.press : .clear))
            .opacity(isEnabled ? 1 : 0.5)
    }
}

/// The session's current-call card: there while a call in session has
/// neither a stage nor the dock (`CallSurface.sidebarCard`).
struct CurrentCallCardHost: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(CallStageModel.self) private var model
    @Environment(\.callMediaControls) private var media
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let state = session.calls.state
        let conversation = model.surface(of: state, open: navigation.selection, platform: model.shellPlatform) == .sidebarCard
            ? state.sessionConversation : nil
        ZStack {
            if let conversation, let sid = state.sid {
                card(for: conversation, sid: sid, state: state)
                    .padding(.horizontal, Theme.Spacing.m)
                    .padding(.vertical, Theme.Spacing.s)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .huddleAnimation(.settle, value: conversation)
    }

    private func card(for conversation: ConversationID, sid: String, state: CallState) -> some View {
        let header = ConversationHeaderText.make(for: conversation, session: session)
        let status: CallCardSummary.Status = switch media?.phase ?? .idle {
        case .idle, .connecting: .connecting
        case .connected: .connected
        case .reconnecting: .reconnecting
        }
        let kind: CallCardSummary.Kind = switch state {
        case .active(let call): .direct(call.media)
        default: .room(count: session.calls.groupPresence.participants[conversation.jid]?.count ?? 0)
        }
        let commands = CallCommands(calls: session.calls, media: media)
        return CurrentCallCard(
            title: conversation.isRoom ? header.title : header.name,
            summary: CallCardSummary.line(status: status, kind: kind),
            isConnected: status == .connected,
            hasMedia: status != .connecting,
            isMicrophoneOn: media?.isMicrophoneOn ?? false,
            isCameraOn: media?.isCameraOn ?? false,
            actions: .init(
                returnToCall: { [model, navigation] in
                    // As the web does: back to the call as a split beside its conversation.
                    model.update(sid) { $0.size = .split }
                    navigation.open(conversation)
                },
                expand: { [model, navigation] in
                    model.update(sid) { $0.size = .expanded }
                    navigation.open(conversation)
                },
                toggleMicrophone: commands.toggleMicrophone,
                toggleCamera: commands.toggleCamera,
                hangUp: commands.hangUp
            )
        )
    }
}
