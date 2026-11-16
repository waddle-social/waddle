import SwiftUI
import WaddleKit

/// The signed-in session's call dock: shows the call slot through
/// `CallDockView`, runs the in-call clock, and answers through the store.
/// Absent while the slot is idle.
struct CallDock: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.callMediaControls) private var media
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let stage = CallStage(state: session.calls.state, media: media?.phase ?? .idle)
        ZStack {
            if let stage {
                clocked(stage)
                    .frame(maxWidth: CallDockView.maxWidth)
                    .padding(.horizontal, Theme.Spacing.m)
                    .padding(.vertical, Theme.Spacing.s)
                    .frame(maxWidth: .infinity)
                    .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
            }
        }
        .huddleAnimation(.settle, value: stage == nil)
    }

    @ViewBuilder
    private func clocked(_ stage: CallStage) -> some View {
        // The media session keeps the start time, so every dock on screen
        // shows the same clock.
        if stage.isInSession, let connectedAt = media?.connectedSince {
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                dock(stage, elapsed: .seconds(context.date.timeIntervalSince(connectedAt)))
            }
        } else {
            dock(stage, elapsed: nil)
        }
    }

    private func dock(_ stage: CallStage, elapsed: Duration?) -> some View {
        CallDockView(
            stage: stage,
            peerName: stage.peer.map { ConversationHeaderText.make(for: .direct($0), session: session).name } ?? "",
            peerJID: stage.peer,
            elapsed: elapsed,
            isMicrophoneOn: media?.isMicrophoneOn ?? false,
            isCameraOn: media?.isCameraOn ?? false,
            remoteVideo: media?.remoteVideo,
            localVideo: media?.localVideo,
            actions: actions
        )
    }

    private var actions: CallDockActions {
        let calls = session.calls
        let media = media
        return CallDockActions(
            accept: { Task { _ = await calls.acceptIncoming() } },
            decline: { Task { _ = await calls.declineIncoming() } },
            hangUp: { Task { await calls.hangUp() } },
            toggleMicrophone: {
                guard let media else { return }
                Task { await media.setMicrophone(!media.isMicrophoneOn) }
            },
            toggleCamera: {
                guard let media else { return }
                Task { await media.setCamera(!media.isCameraOn) }
            },
            dismiss: { calls.dismiss() }
        )
    }
}
