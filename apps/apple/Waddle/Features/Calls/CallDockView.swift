import SwiftUI
import WaddleKit

/// What the call dock's controls do.
struct CallDockActions {
    let accept: @MainActor () -> Void
    let decline: @MainActor () -> Void
    let hangUp: @MainActor () -> Void
    let toggleMicrophone: @MainActor () -> Void
    let toggleCamera: @MainActor () -> Void
    let dismiss: @MainActor () -> Void
    /// Opens the call's stage over the screen, on a phone; `nil` where there is no such stage.
    var open: (@MainActor () -> Void)?
}

/// The call surface, from the first ring to the reason the call ended.
/// Pure: everything it shows is passed in, so it renders without a session.
///
/// It lays out in one row when that fits, and stacks the controls under
/// the caller otherwise (an iPhone, a narrow window, large Dynamic Type).
struct CallDockView: View {
    let stage: CallStage
    let peerName: String
    var peerJID: BareJID?
    /// Time since media first connected; shown while in call.
    var elapsed: Duration?
    var isMicrophoneOn = false
    var isCameraOn = false
    var video = CallDockVideo()
    /// Something the person should know about their own devices.
    var notice: String?
    /// Whether this dock speaks stage changes; one dock per screen does.
    var announcesChanges = true
    let actions: CallDockActions

    @ScaledMetric(relativeTo: .body) private var endedGlyphSize: CGFloat = 17

    var body: some View {
        HuddleCard(isLive: stage.isInSession) {
            VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                if showsVideoStage {
                    videoStage
                }
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Theme.Spacing.m) {
                        caller
                        Spacer(minLength: Theme.Spacing.s)
                        controls(stretched: false)
                    }
                    VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                        caller
                        controls(stretched: true)
                    }
                }
            }
        }
        .environment(\.huddleGround, Theme.Colors.surface)
        .huddleAnimation(.settle, value: stage)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call"))
        .onChange(of: announcement, initial: true) { _, text in
            if announcesChanges { AccessibilityNotification.Announcement(text).post() }
        }
    }

    // MARK: - Caller

    private var caller: some View {
        HStack(spacing: Theme.Spacing.m) {
            if case .ended = stage {
                Image(systemName: "phone.down.fill")
                    .font(.system(size: endedGlyphSize, weight: .semibold))
                    .foregroundStyle(Theme.Colors.fgSoft)
                    .frame(width: Theme.Size.callAvatar, height: Theme.Size.callAvatar)
                    .background(Circle().fill(Theme.Colors.surface2))
                    .accessibilityHidden(true)
            } else if !showsVideoStage {
                avatar(size: Theme.Size.callAvatar)
            }
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                if case .ended(let reason) = stage {
                    Text(reason.summary)
                        .huddleText(Theme.Typography.label)
                        .foregroundStyle(Theme.Colors.fg)
                } else {
                    Text(peerName)
                        .huddleText(Theme.Typography.subtitle)
                        .foregroundStyle(Theme.Colors.fg)
                    status
                    if let notice, stage.isInSession {
                        Text(notice)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.warningText)
                    }
                }
            }
            .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private func avatar(size: CGFloat) -> some View {
        if let peerJID {
            JIDAvatar(jid: peerJID, name: peerName, size: size)
        } else {
            AvatarView(name: peerName, size: size)
        }
    }

    private var status: some View {
        CallStatusLine(text: statusText, isLive: stageIsInCall, isEmphasised: stage.isInSession, color: statusColor)
    }

    private var statusText: String {
        switch stage {
        case .incoming(_, _, true): "Connecting…"
        case .incoming(_, let media, false): media.video ? "Incoming video call" : "Incoming call"
        case .calling: "Calling…"
        case .ringing: "Ringing…"
        case .connecting: "Connecting…"
        case .inCall: elapsed.map(CallClock.string(for:)) ?? "In call"
        case .reconnecting: "Reconnecting…"
        case .ended(let reason): reason.summary
        }
    }

    private var statusColor: Color {
        switch stage {
        case .inCall: Theme.Colors.liveText
        case .reconnecting: Theme.Colors.warningText
        default: Theme.Colors.fgMuted
        }
    }

    private var stageIsInCall: Bool {
        if case .inCall = stage { true } else { false }
    }

    /// What VoiceOver says when the stage changes; never the ticking clock.
    private var announcement: String {
        switch stage {
        case .incoming(_, let media, false): media.video ? "Incoming video call from \(peerName)" : "Incoming call from \(peerName)"
        case .incoming(_, _, true), .connecting: "Connecting"
        case .calling: "Calling \(peerName)"
        case .ringing: "Ringing"
        case .inCall: "Call connected"
        case .reconnecting: "Reconnecting"
        case .ended(let reason): reason.summary
        }
    }

    // MARK: - Controls

    /// Inline, the controls keep their own width. Stacked under the caller
    /// they share the card's width, and go one per row once their labels
    /// no longer fit side by side.
    @ViewBuilder
    private func controls(stretched: Bool) -> some View {
        if stretched {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: Theme.Spacing.s) { buttons(stretched: true) }
                VStack(spacing: Theme.Spacing.s) {
                    if stage.isInSession {
                        HStack(spacing: Theme.Spacing.s) {
                            microphoneToggle(stretched: true)
                            cameraToggle(stretched: true)
                            CallOpenButton(stretched: true, action: actions.open)
                        }
                        endButton(stretched: true)
                    } else {
                        buttons(stretched: true)
                    }
                }
            }
        } else {
            HStack(spacing: Theme.Spacing.s) { buttons(stretched: false) }
        }
    }

    @ViewBuilder
    private func buttons(stretched: Bool) -> some View {
        switch stage {
        case .incoming(_, let media, let isAccepting):
            labelled(isAccepting ? "Cancel" : "Decline", symbol: "phone.down.fill", .danger, stretched, actions.decline)
            labelled("Accept", symbol: media.video ? "video.fill" : "phone.fill", .live, stretched, actions.accept)
                .disabled(isAccepting)
        case .calling, .ringing:
            labelled("Cancel", symbol: "phone.down.fill", .danger, stretched, actions.hangUp)
        case .connecting, .inCall, .reconnecting:
            microphoneToggle(stretched: false)
            cameraToggle(stretched: false)
            CallOpenButton(action: actions.open)
            endButton(stretched: stretched)
        case .ended:
            Button("Dismiss", action: actions.dismiss)
                .buttonStyle(.huddle())
        }
    }

    private func labelled(
        _ title: String, symbol: String, _ variant: HuddleButtonStyle.Variant, _ stretched: Bool,
        _ action: @escaping @MainActor () -> Void
    ) -> some View {
        CallLabelledButton(title: title, symbol: symbol, variant: variant, stretched: stretched, action: action)
    }

    private func endButton(stretched: Bool) -> some View {
        labelled("End", symbol: "phone.down.fill", .danger, stretched, actions.hangUp)
            .accessibilityLabel(Text("End call"))
            .help("End call")
    }

    private func microphoneToggle(stretched: Bool) -> some View {
        CallIconToggle(
            title: isMicrophoneOn ? "Mute" : "Unmute", symbol: isMicrophoneOn ? "mic.fill" : "mic.slash.fill",
            tint: isMicrophoneOn ? Theme.Colors.fg : Theme.Colors.dangerText, stretched: stretched,
            action: actions.toggleMicrophone
        )
        .disabled(!hasMedia)
    }

    private func cameraToggle(stretched: Bool) -> some View {
        CallIconToggle(
            title: isCameraOn ? "Turn camera off" : "Turn camera on", symbol: isCameraOn ? "video.fill" : "video.slash.fill",
            stretched: stretched, action: actions.toggleCamera
        )
        .disabled(!hasMedia)
    }

    /// While the media session is still connecting, the devices are about
    /// to start on their own; a toggle then would promise the opposite.
    private var hasMedia: Bool {
        switch stage {
        case .inCall, .reconnecting: true
        default: false
        }
    }

    // MARK: - Video

    private var showsVideoStage: Bool {
        stage.isInSession && video.hasCamera
    }

    /// The peer's camera, or their avatar while it is off.
    private var videoStage: some View {
        CallVideoStage(
            video: video,
            label: video.screenSharer != nil ? "\(peerName)’s screen"
                : video.remote == nil ? "\(peerName)’s camera is off" : "\(peerName)’s video"
        ) {
            avatar(size: Theme.Size.callStageAvatar)
        }
    }

    /// The widest card: at this width a height-capped stage still fills it.
    static let maxWidth = CallVideoMetrics.stageHeight * 16 / 9 + 2 * Theme.Spacing.l
}
