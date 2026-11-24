import SwiftUI
import WaddleKit

/// What the group call dock's controls do.
struct GroupCallDockActions {
    let leave: @MainActor () -> Void
    let toggleMicrophone: @MainActor () -> Void
    let toggleCamera: @MainActor () -> Void
    let toggleHand: @MainActor () -> Void
}

/// A room's call in the dock: the room, who is in it, and our controls.
/// Pure: everything it shows is passed in, so it renders without a session.
///
/// Like `CallDockView` it puts the controls beside the room when they fit
/// and under it otherwise. The faces always have a row of their own, so the
/// layout does not change as people come and go.
struct GroupCallDockView: View {
    let stage: GroupCallStage
    let roomName: String
    /// The room's mark: `number` for a channel (see `ChannelSymbol`).
    var roomSymbol = "number"
    /// Who is in the call, ours first (`GroupCallRoster`).
    let rows: [GroupCallRosterRow]
    /// Time since media first connected; shown while in call.
    var elapsed: Duration?
    var isMicrophoneOn = false
    var isCameraOn = false
    var isHandRaised = false
    var video = CallDockVideo()
    /// Something the person should know about their own devices.
    var notice: String?
    /// Whether this dock speaks stage changes; one dock per screen does.
    var announcesChanges = true
    let actions: GroupCallDockActions

    @ScaledMetric(relativeTo: .body) private var markGlyph: CGFloat = 17
    @ScaledMetric(relativeTo: .body) private var stageMarkGlyph: CGFloat = 28

    var body: some View {
        HuddleCard(isLive: isAccepted) {
            VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                if showsVideoStage {
                    videoStage
                }
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Theme.Spacing.m) {
                        header
                        Spacer(minLength: Theme.Spacing.s)
                        controls(stretched: false)
                    }
                    VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                        header
                        controls(stretched: true)
                    }
                }
                if showsRoster {
                    roster
                }
            }
        }
        .environment(\.huddleGround, Theme.Colors.surface)
        .huddleAnimation(.settle, value: stage)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call in \(roomName)"))
        .onChange(of: announcement, initial: true) { _, text in
            if announcesChanges { AccessibilityNotification.Announcement(text).post() }
        }
    }

    // MARK: - Room

    /// Edge to edge with the room: the room each face keeps for its ring and
    /// badges hangs into the card's padding.
    private var roster: some View {
        GroupCallRosterStrip(rows: rows)
            .padding(-GroupCallRosterFace.ringInset)
    }

    private var header: some View {
        HStack(spacing: Theme.Spacing.m) {
            if !showsVideoStage {
                roomMark(size: Theme.Size.callAvatar, glyph: markGlyph)
            }
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(roomName)
                    .huddleText(Theme.Typography.subtitle)
                    .foregroundStyle(Theme.Colors.fg)
                status
                if let notice, isAccepted {
                    Text(notice)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.warningText)
                }
            }
            .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }

    private func roomMark(size: CGFloat, glyph: CGFloat) -> some View {
        Image(systemName: roomSymbol)
            // Large text grows the glyph, never past the mark.
            .font(.system(size: min(glyph, size / 2), weight: .semibold))
            .foregroundStyle(isAccepted ? Theme.Colors.liveText : Theme.Colors.fgSoft)
            .frame(width: size, height: size)
            .background(Circle().fill(isAccepted ? Theme.Colors.liveSubtle : Theme.Colors.surface2))
            .accessibilityHidden(true)
    }

    /// The status, then who is there; the count moves under the status
    /// when both no longer fit on one line.
    @ViewBuilder
    private var status: some View {
        let line = CallStatusLine(text: statusText, isLive: isInCall, isEmphasised: isAccepted, color: statusColor)
        if let people {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: Theme.Spacing.xs) {
                    line
                    Text("·")
                        .huddleText(Theme.Typography.label.weight(400))
                        .foregroundStyle(Theme.Colors.fgSoft)
                        .accessibilityHidden(true)
                    peopleText(people)
                }
                .fixedSize()
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    line
                    peopleText(people)
                }
            }
        } else {
            line
        }
    }

    private func peopleText(_ text: String) -> some View {
        Text(text)
            .huddleText(Theme.Typography.label.weight(400))
            .foregroundStyle(Theme.Colors.fgMuted)
    }

    private var statusText: String {
        switch stage {
        case .joining: "Joining…"
        case .connecting: "Connecting…"
        case .inCall: elapsed.map(CallClock.string(for:)) ?? "In call"
        case .reconnecting: "Reconnecting…"
        }
    }

    private var statusColor: Color {
        switch stage {
        case .inCall: Theme.Colors.liveText
        case .reconnecting: Theme.Colors.warningText
        case .joining, .connecting: Theme.Colors.fgMuted
        }
    }

    /// Alone, the status line says so and our own controls show our state.
    private var showsRoster: Bool {
        !rows.isEmpty && !(rows.count == 1 && rows[0].isSelf)
    }

    /// Nobody listed yet says nothing; only us says so plainly.
    private var people: String? {
        switch rows.count {
        case 0: nil
        case 1 where rows[0].isSelf: "Just you"
        case 1: "1 person"
        case let count: "\(count) people"
        }
    }

    private var isInCall: Bool {
        if case .inCall = stage { true } else { false }
    }

    /// The mixer accepted us: the call is ours, not just being joined.
    private var isAccepted: Bool {
        if case .joining = stage { false } else { true }
    }

    /// What VoiceOver says when the stage changes; never the ticking clock.
    private var announcement: String {
        switch stage {
        case .joining: "Joining call in \(roomName)"
        case .connecting: "Connecting"
        case .inCall: "Call connected"
        case .reconnecting: "Reconnecting"
        }
    }

    // MARK: - Controls

    /// Inline, the controls keep their own width. Stacked under the room
    /// they share the card's width, and Leave takes a row of its own once
    /// the four no longer fit side by side.
    @ViewBuilder
    private func controls(stretched: Bool) -> some View {
        if stretched {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: Theme.Spacing.s) { buttons(stretched: true) }
                VStack(spacing: Theme.Spacing.s) {
                    HStack(spacing: Theme.Spacing.s) {
                        microphoneToggle(stretched: true)
                        cameraToggle(stretched: true)
                        handToggle(stretched: true)
                    }
                    leaveButton(stretched: true)
                }
            }
        } else {
            HStack(spacing: Theme.Spacing.s) { buttons(stretched: false) }
        }
    }

    @ViewBuilder
    private func buttons(stretched: Bool) -> some View {
        microphoneToggle(stretched: false)
        cameraToggle(stretched: false)
        handToggle(stretched: false)
        leaveButton(stretched: stretched)
    }

    private func leaveButton(stretched: Bool) -> some View {
        CallLabelledButton(title: "Leave", symbol: "phone.down.fill", variant: .danger, stretched: stretched, action: actions.leave)
            .accessibilityLabel(Text("Leave call"))
            .help("Leave call")
    }

    private func microphoneToggle(stretched: Bool) -> some View {
        CallIconToggle(
            title: isMicrophoneOn ? "Mute" : "Unmute", symbol: isMicrophoneOn ? "mic.fill" : "mic.slash.fill",
            tint: isMicrophoneOn ? Theme.Colors.fg : Theme.Colors.dangerText, stretched: stretched,
            action: actions.toggleMicrophone
        )
        .disabled(!stage.hasMedia)
    }

    private func cameraToggle(stretched: Bool) -> some View {
        CallIconToggle(
            title: isCameraOn ? "Turn camera off" : "Turn camera on", symbol: isCameraOn ? "video.fill" : "video.slash.fill",
            stretched: stretched, action: actions.toggleCamera
        )
        .disabled(!stage.hasMedia)
    }

    private func handToggle(stretched: Bool) -> some View {
        CallIconToggle(
            title: isHandRaised ? "Lower hand" : "Raise hand", symbol: isHandRaised ? "hand.raised.fill" : "hand.raised",
            tint: isHandRaised ? Theme.Colors.goldText : Theme.Colors.fg, stretched: stretched,
            action: actions.toggleHand
        )
        .accessibilityAddTraits(isHandRaised ? .isSelected : [])
        .disabled(!stage.hasMedia)
    }

    // MARK: - Video

    private var showsVideoStage: Bool {
        isAccepted && video.hasCamera
    }

    /// One camera from the room, or the room's mark while none is on.
    private var videoStage: some View {
        CallVideoStage(
            video: video,
            label: video.screenSharer.map { "\($0)’s screen" }
                ?? (video.remote == nil ? "Nobody else’s camera is on" : "A camera in the call")
        ) {
            roomMark(size: Theme.Size.callStageAvatar, glyph: stageMarkGlyph)
        }
    }
}
