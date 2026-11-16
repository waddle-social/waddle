import SwiftUI
import WaddleKit

/// The signed-in session's call dock: shows the call slot through
/// `CallDockView`, runs the in-call clock, and answers through the store.
/// Absent while the slot is idle.
struct CallDock: View {
    /// Absent where a presentation was given no session.
    @Environment(SessionCoordinator.self) private var session: SessionCoordinator?
    @Environment(\.callMediaControls) private var media
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Off for the docks inside sheets, so a stage change is spoken once.
    var announcesChanges = true

    var body: some View {
        let state = session?.calls.state ?? .idle
        let stage = CallStage(state: state, media: media?.phase ?? .idle)
        ZStack {
            if let stage, let session {
                clocked(stage, in: session)
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
    private func clocked(_ stage: CallStage, in session: SessionCoordinator) -> some View {
        // The media session keeps the start time, so every dock on screen
        // shows the same clock.
        if stage.isInSession, let connectedAt = media?.connectedSince {
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                dock(stage, elapsed: .seconds(context.date.timeIntervalSince(connectedAt)), in: session)
            }
        } else {
            dock(stage, elapsed: nil, in: session)
        }
    }

    private func dock(_ stage: CallStage, elapsed: Duration?, in session: SessionCoordinator) -> some View {
        CallDockView(
            stage: stage,
            peerName: stage.peer.map { ConversationHeaderText.make(for: .direct($0), session: session).name } ?? "",
            peerJID: stage.peer,
            elapsed: elapsed,
            isMicrophoneOn: media?.isMicrophoneOn ?? false,
            isCameraOn: media?.isCameraOn ?? false,
            remoteVideo: media?.remoteVideo,
            localVideo: media?.localVideo,
            notice: notice,
            announcesChanges: announcesChanges,
            actions: actions(for: session.calls)
        )
    }

    private var notice: String? {
        guard let media else { return nil }
        switch (media.isMicrophoneBlocked, media.isCameraBlocked) {
        case (true, true): return "Waddle is not allowed to use the microphone or camera. Change that in Settings."
        case (true, false): return "Waddle is not allowed to use the microphone. Change that in Settings."
        case (false, true): return "Waddle is not allowed to use the camera. Change that in Settings."
        case (false, false): return nil
        }
    }

    /// Each control acts only on the call it was drawn for: a slot that has
    /// since moved to another call ignores a tap meant for the last one.
    private func actions(for calls: CallStore) -> CallDockActions {
        let sid = calls.state.sid
        let media = media
        func onThisCall(_ action: @escaping @MainActor () async -> Void) -> @MainActor () -> Void {
            {
                guard calls.state.sid == sid else { return }
                Task {
                    guard calls.state.sid == sid else { return }
                    await action()
                }
            }
        }
        return CallDockActions(
            accept: onThisCall { _ = await calls.acceptIncoming() },
            decline: onThisCall { _ = await calls.declineIncoming() },
            hangUp: onThisCall { await calls.hangUp() },
            toggleMicrophone: onThisCall {
                guard let media else { return }
                await media.setMicrophone(!media.isMicrophoneOn)
            },
            toggleCamera: onThisCall {
                guard let media else { return }
                await media.setCamera(!media.isCameraOn)
            },
            dismiss: onThisCall { calls.dismiss() }
        )
    }
}

extension View {
    /// A sheet or a cover hides the shell's dock, so it carries its own: a
    /// call that arrives while it is open is still seen and can be answered.
    func carryingCallDock() -> some View {
        VStack(spacing: 0) {
            CallDock(announcesChanges: false)
            self
        }
    }
}
