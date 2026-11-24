import SwiftUI
import WaddleKit

/// The signed-in session's call dock: shows the call slot through
/// `CallDockView`, or `GroupCallDockView` for a room's call, runs the
/// in-call clock, and answers through the store. Absent while the slot is
/// idle, and while the call has a stage of its own beside its conversation
/// (`CallSurface`): one surface draws a call.
struct CallDock: View {
    /// Absent where a presentation was given no session.
    @Environment(SessionCoordinator.self) private var session: SessionCoordinator?
    @Environment(NavigationModel.self) private var navigation: NavigationModel?
    @Environment(\.callMediaControls) private var media
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Absent in a sheet given no session; a dock there never yields anyway.
    @Environment(CallStageModel.self) private var stageModel: CallStageModel?
    /// Absent in a sheet, whose dock never yields.
    @Environment(CallWindowLayout.self) private var layout: CallWindowLayout?
    @CallPlatform private var platform
    /// The dock a sheet carries. It stays quiet, so a stage change is spoken
    /// once, and it leaves the video of a call in session to the surface
    /// behind the sheet.
    var isInSheet = false
    @State private var speaking = SpeakingHold()

    var body: some View {
        let state = session?.calls.state ?? .idle
        let phase = media?.phase ?? .idle
        let stage = CallStage(state: state, media: phase)
        let group = GroupCallStage(state: state, media: phase)
        let surface = stageModel?.surface(of: state, open: navigation?.selection, platform: platform, in: layout)
            ?? CallSurface.placement(of: state, open: navigation?.selection, platform: platform, pane: nil)
        let content = surface
            .dockContent(isInSession: state.sessionConversation != nil, isInSheet: isInSheet)
        let shown = content == .hidden ? nil : stage
        let speakers = (media?.participants ?? []).lazy.filter(\.isSpeaking).map(\.identity)
        ZStack {
            if let shown, let session {
                clocked(shown, group: group, showsVideo: content == .call, in: session)
                    .frame(maxWidth: CallDockView.maxWidth)
                    .padding(.horizontal, Theme.Spacing.m)
                    .padding(.vertical, Theme.Spacing.s)
                    .frame(maxWidth: .infinity)
                    .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
            }
        }
        .huddleAnimation(.settle, value: shown == nil)
        .onChange(of: GroupCallRoster.speakers(speakers), initial: true) { _, accounts in
            speaking.update(accounts)
        }
    }

    @ViewBuilder
    private func clocked(_ stage: CallStage, group: GroupCallStage?, showsVideo: Bool, in session: SessionCoordinator) -> some View {
        let video = showsVideo ? dockVideo : CallDockVideo()
        // The media session keeps the start time, so every dock on screen
        // shows the same clock.
        if stage.isInSession, let connectedAt = media?.connectedSince {
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                dock(stage, group: group, elapsed: .seconds(context.date.timeIntervalSince(connectedAt)), video: video, in: session)
            }
        } else {
            dock(stage, group: group, elapsed: nil, video: video, in: session)
        }
    }

    @ViewBuilder
    private func dock(
        _ stage: CallStage, group: GroupCallStage?, elapsed: Duration?, video: CallDockVideo, in session: SessionCoordinator
    ) -> some View {
        let commands = CallCommands(calls: session.calls, media: media)
        if let group {
            GroupCallDockView(
                stage: group,
                roomName: session.directory.title(for: .room(group.room)),
                roomSymbol: session.directory.channel(for: group.room).map(ChannelSymbol.name(for:)) ?? "number",
                rows: session.callRoster(of: group, media: media, speaking: speaking.shown),
                elapsed: elapsed,
                isMicrophoneOn: media?.isMicrophoneOn ?? false,
                isCameraOn: media?.isCameraOn ?? false,
                isHandRaised: session.calls.isHandRaised,
                video: video,
                notice: media?.deviceNotice,
                announcesChanges: !isInSheet,
                actions: GroupCallDockActions(
                    leave: commands.hangUp, toggleMicrophone: commands.toggleMicrophone,
                    toggleCamera: commands.toggleCamera, toggleHand: commands.toggleHand
                )
            )
        } else {
            CallDockView(
                stage: stage,
                peerName: stage.peer.map { ConversationHeaderText.make(for: .direct($0), session: session).name } ?? "",
                peerJID: stage.peer,
                elapsed: elapsed,
                isMicrophoneOn: media?.isMicrophoneOn ?? false,
                isCameraOn: media?.isCameraOn ?? false,
                video: video,
                notice: media?.deviceNotice,
                announcesChanges: !isInSheet,
                actions: CallDockActions(
                    accept: commands.accept, decline: commands.decline, hangUp: commands.hangUp,
                    toggleMicrophone: commands.toggleMicrophone, toggleCamera: commands.toggleCamera,
                    dismiss: commands.dismiss
                )
            )
        }
    }

    private var dockVideo: CallDockVideo {
        guard let media else { return CallDockVideo() }
        let cameras = media.dockCameraIdentities
        let preview = cameras.local.flatMap { media.video(of: $0, source: .camera) }
        // A shared screen is what the call is about while it is shared.
        if let sharer = cameras.remoteScreen, let screen = media.video(of: sharer, source: .screen) {
            return CallDockVideo(
                remote: screen, preview: preview, screenSharer: JID(parsing: sharer)?.bare.localpart ?? sharer
            )
        }
        return CallDockVideo(remote: cameras.remote.flatMap { media.video(of: $0, source: .camera) }, preview: preview)
    }
}

extension View {
    /// A sheet or a cover hides the shell's dock, so it carries its own: a
    /// call that arrives while it is open is still seen and can be answered.
    func carryingCallDock() -> some View {
        VStack(spacing: 0) {
            CallDock(isInSheet: true)
            self
        }
    }
}
