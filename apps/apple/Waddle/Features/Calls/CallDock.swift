import SwiftUI
import WaddleKit

/// The signed-in session's call dock: shows the call slot through
/// `CallDockView`, or `GroupCallDockView` for a room's call, runs the
/// in-call clock, and answers through the store. Absent while the slot is
/// idle.
struct CallDock: View {
    /// Absent where a presentation was given no session.
    @Environment(SessionCoordinator.self) private var session: SessionCoordinator?
    @Environment(\.callMediaControls) private var media
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Off for the docks inside sheets, so a stage change is spoken once.
    var announcesChanges = true
    @State private var speaking = SpeakingHold()

    var body: some View {
        let state = session?.calls.state ?? .idle
        let phase = media?.phase ?? .idle
        let stage = CallStage(state: state, media: phase)
        let group = GroupCallStage(state: state, media: phase)
        ZStack {
            if let stage, let session {
                clocked(stage, group: group, in: session)
                    .frame(maxWidth: CallDockView.maxWidth)
                    .padding(.horizontal, Theme.Spacing.m)
                    .padding(.vertical, Theme.Spacing.s)
                    .frame(maxWidth: .infinity)
                    .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
            }
        }
        .huddleAnimation(.settle, value: stage == nil)
        .onChange(of: media?.speakingIdentities ?? [], initial: true) { _, identities in
            speaking.update(GroupCallRoster.speakers(identities))
        }
    }

    @ViewBuilder
    private func clocked(_ stage: CallStage, group: GroupCallStage?, in session: SessionCoordinator) -> some View {
        // The media session keeps the start time, so every dock on screen
        // shows the same clock.
        if stage.isInSession, let connectedAt = media?.connectedSince {
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                dock(stage, group: group, elapsed: .seconds(context.date.timeIntervalSince(connectedAt)), in: session)
            }
        } else {
            dock(stage, group: group, elapsed: nil, in: session)
        }
    }

    @ViewBuilder
    private func dock(_ stage: CallStage, group: GroupCallStage?, elapsed: Duration?, in session: SessionCoordinator) -> some View {
        if let group {
            groupDock(group, elapsed: elapsed, in: session)
        } else {
            directDock(stage, elapsed: elapsed, in: session)
        }
    }

    private func directDock(_ stage: CallStage, elapsed: Duration?, in session: SessionCoordinator) -> some View {
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

    private func groupDock(_ stage: GroupCallStage, elapsed: Duration?, in session: SessionCoordinator) -> some View {
        let calls = session.calls
        let room = stage.room
        // The SFU's list is the truth only while its media is up.
        let live = stage.hasMedia ? (media?.participantIdentities ?? []).compactMap(JID.init(parsing:)) : []
        return GroupCallDockView(
            stage: stage,
            roomName: session.directory.title(for: .room(room)),
            roomSymbol: session.directory.channel(for: room).map(ChannelSymbol.name(for:)) ?? "number",
            rows: GroupCallRoster.rows(
                presence: calls.groupPresence.participants[room] ?? [], live: live, speaking: speaking.shown,
                own: session.account.jid
            ),
            elapsed: elapsed,
            isMicrophoneOn: media?.isMicrophoneOn ?? false,
            isCameraOn: media?.isCameraOn ?? false,
            isHandRaised: calls.isHandRaised,
            remoteVideo: media?.remoteVideo,
            localVideo: media?.localVideo,
            notice: notice,
            announcesChanges: announcesChanges,
            actions: groupActions(for: calls)
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
    private static func onThisCall(_ calls: CallStore) -> (@escaping @MainActor () async -> Void) -> @MainActor () -> Void {
        let sid = calls.state.sid
        return { action in
            {
                guard calls.state.sid == sid else { return }
                Task {
                    guard calls.state.sid == sid else { return }
                    await action()
                }
            }
        }
    }

    private func actions(for calls: CallStore) -> CallDockActions {
        let onThisCall = Self.onThisCall(calls)
        return CallDockActions(
            accept: onThisCall { _ = await calls.acceptIncoming() },
            decline: onThisCall { _ = await calls.declineIncoming() },
            hangUp: onThisCall { await calls.hangUp() },
            toggleMicrophone: onThisCall(toggleMicrophone(on: calls)),
            toggleCamera: onThisCall(toggleCamera(on: calls)),
            dismiss: onThisCall { calls.dismiss() }
        )
    }

    private func groupActions(for calls: CallStore) -> GroupCallDockActions {
        let onThisCall = Self.onThisCall(calls)
        return GroupCallDockActions(
            leave: onThisCall { await calls.hangUp() },
            toggleMicrophone: onThisCall(toggleMicrophone(on: calls)),
            toggleCamera: onThisCall(toggleCamera(on: calls)),
            toggleHand: onThisCall {
                guard case .inGroup = calls.state else { return }
                _ = await calls.setHandRaised(!calls.isHandRaised)
            }
        )
    }

    /// A device choice belongs to a session in progress; a tap that lands
    /// as the call ends must not become the next call's choice.
    private func toggleMicrophone(on calls: CallStore) -> @MainActor () async -> Void {
        let media = media
        return {
            guard let media, calls.state.isEstablished else { return }
            await media.setMicrophone(!media.isMicrophoneOn)
        }
    }

    /// As `toggleMicrophone(on:)`.
    private func toggleCamera(on calls: CallStore) -> @MainActor () async -> Void {
        let media = media
        return {
            guard let media, calls.state.isEstablished else { return }
            await media.setCamera(!media.isCameraOn)
        }
    }
}

private extension CallState {
    /// A device choice belongs to a session in progress: an accepted 1:1
    /// session or a room's call the mixer accepted.
    var isEstablished: Bool {
        switch self {
        case .active, .inGroup: true
        case .idle, .incoming, .outgoing, .joiningGroup, .ended: false
        }
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
