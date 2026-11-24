import SwiftUI
import WaddleKit

/// What the person chose on the stage of the session's call: the view, a
/// pin, their own view hidden. It belongs to the session, not to a view, so
/// it is still there when they come back to the call's conversation.
@MainActor @Observable
final class CallStageModel {
    private var memory = CallStageMemory()
    /// The pane a stage shares with its conversation, once the shell has
    /// laid it out: the dock and the stage both place the call by it.
    var pane: CGSize?
    @ObservationIgnored private(set) var tileSource = CallTileSource(video: { _ in nil }, togglePin: { _ in /* no call yet */ })
    /// The share of the pane each conversation's stage was dragged to, kept
    /// on this device (web keeps it per room in local storage).
    private var splitPercents: [String: Double]
    @ObservationIgnored private let defaults: UserDefaults
    private static let splitPercentsKey = "callSplitPercents"

    /// - Parameter media: where tiles get their pictures; `nil` draws avatars only.
    init(calls: CallStore, media: (any CallMediaControls)?, defaults: UserDefaults = .standard) {
        self.defaults = defaults
        splitPercents = defaults.dictionary(forKey: Self.splitPercentsKey)?.compactMapValues { $0 as? Double } ?? [:]
        tileSource = CallTileSource(
            video: { [weak media] tile in media?.video(of: tile.identity, source: tile.source) },
            // A pin is for whatever call is on the stage when the tile is clicked.
            togglePin: { [weak self, weak calls] tile in
                guard let self, let sid = calls?.state.sid else { return }
                update(sid) { $0.togglePin(tile) }
            }
        )
    }

    /// The choices for call `sid`; a call nobody has touched has the defaults.
    func stage(of sid: String) -> CallStageMemory {
        var memory = memory
        memory.follow(call: sid)
        return memory
    }

    /// Where the slot's call shows while `selection` is the open conversation.
    func surface(of state: CallState, open selection: ConversationID?, platform: CallSurface.Platform) -> CallSurface {
        CallSurface.placement(
            of: state, open: selection, platform: platform, pane: pane,
            size: state.sid.map { stage(of: $0).size } ?? .defaultSize
        )
    }

    func splitPercent(for conversation: ConversationID) -> Double? {
        splitPercents[conversation.description]
    }

    func setSplitPercent(_ percent: Double, for conversation: ConversationID) {
        let percent = CallSurface.clampSplitPercent(percent)
        guard splitPercents[conversation.description] != percent else { return }
        splitPercents[conversation.description] = percent
    }

    /// Once a drag is over, not on each point of it.
    func saveSplitPercents() {
        defaults.set(splitPercents, forKey: Self.splitPercentsKey)
    }

    /// Writes only a change, so an update that changes nothing redraws nothing.
    func update(_ sid: String, _ change: (inout CallStageMemory) -> Void) {
        var next = memory
        next.follow(call: sid)
        change(&next)
        if next != memory { memory = next }
    }
}

/// The stage of the session's call in progress: reads the call slot, the
/// media session and the room's roster into `CallStageView`, and sends its
/// controls to the store. Draws nothing unless a call is in session.
struct CallStageHost: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(CallStageModel.self) private var model
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.callMediaControls) private var media
    @State private var speaking = SpeakingHold()

    var body: some View {
        let state = session.calls.state
        let speakers = (media?.participants ?? []).lazy.filter(\.isSpeaking).map(\.identity)
        Group {
            if let call = Call(state: state, phase: media?.phase ?? .idle, session: session) {
                stage(of: call)
            }
        }
        .onChange(of: GroupCallRoster.speakers(speakers), initial: true) { _, accounts in
            speaking.update(accounts)
        }
    }

    private func stage(of call: Call) -> some View {
        let live = (media?.participants ?? []).map {
            CallStageParticipant(identity: $0.identity, isSelf: $0.isLocal, hasCamera: $0.isCameraOn)
        }
        // The microphone starts a moment after media connects; until then it is not a mute.
        let isSelfMuted = call.status != .connecting && media?.isMicrophoneOn == false
        let tiles: [CallStageTile]
        let subtitle: String
        switch call.kind {
        case .direct(let peer):
            tiles = CallStageTiles.direct(
                live: live, own: call.own, peer: peer, peerName: call.title, isSelfMuted: isSelfMuted,
                speaking: speaking.shown
            )
            subtitle = CallStageSubtitle.direct(call.media)
        case .room(let stage):
            let rows = session.callRoster(of: stage, media: media, speaking: speaking.shown)
            tiles = CallStageTiles.room(live: live, own: call.own, rows: rows, isSelfMuted: isSelfMuted)
            subtitle = CallStageSubtitle.room(others: rows.count { !$0.isSelf })
        }
        let memory = model.stage(of: call.sid)
        return CallStageView(
            title: call.title,
            subtitle: subtitle,
            status: status(of: call),
            tiles: tiles,
            presentation: memory.presentation,
            promotedSpeaker: memory.promotedSpeaker,
            size: memory.size,
            // An open inspector has a composer whose Escape cancels a reply or an edit.
            escapeStepsDown: memory.size == .immersive || navigation.inspector == nil,
            isMicrophoneOn: media?.isMicrophoneOn ?? false,
            isCameraOn: media?.isCameraOn ?? false,
            isHandRaised: call.isRoom ? session.calls.isHandRaised : nil,
            notice: media?.deviceNotice,
            tileSource: model.tileSource,
            actions: actions(for: call.sid)
        )
        // Keyed on who has a tile, not on what the tiles show: a pin does not care who is speaking.
        .onChange(
            of: Roster(tiles: tiles.map(\.id), isAvailable: !live.isEmpty, isSelfViewHidden: memory.presentation.isSelfViewHidden),
            initial: true
        ) { _, roster in
            // During a reconnect nobody is live and the tiles are stand-ins.
            model.update(call.sid) { $0.tilesChanged(tiles, rosterIsAvailable: roster.isAvailable) }
        }
        .onChange(of: media?.activeSpeakersLoudestFirst ?? [], initial: true) { _, speakers in
            model.update(call.sid) { $0.speakersChanged(speakers) }
        }
    }

    /// The media session keeps when it connected, so the stage and a dock show one clock.
    private func status(of call: Call) -> CallStageStatus {
        call.status == .inCall(since: nil) ? .inCall(since: media?.connectedSince) : call.status
    }

    private func actions(for sid: String) -> CallStageActions {
        let commands = CallCommands(calls: session.calls, media: media)
        return CallStageActions(
            toggleMicrophone: commands.toggleMicrophone,
            toggleCamera: commands.toggleCamera,
            toggleHand: commands.toggleHand,
            setViewMode: { [model] mode in model.update(sid) { $0.presentation.viewMode = mode } },
            toggleSelfView: { [model] in model.update(sid) { $0.presentation.isSelfViewHidden.toggle() } },
            setSize: { [model] size in model.update(sid) { $0.size = size } },
            hangUp: commands.hangUp
        )
    }

    /// What the pin is reconciled against.
    private struct Roster: Equatable {
        let tiles: [CallStageTile.ID]
        let isAvailable: Bool
        let isSelfViewHidden: Bool
    }

    /// The slot's call in session, as the stage reads it.
    private struct Call {
        enum Kind {
            /// The peer's device the session was set up with.
            case direct(peer: JID)
            case room(GroupCallStage)
        }

        let sid: String
        let kind: Kind
        var isRoom: Bool { if case .room = kind { true } else { false } }
        let title: String
        let media: CallMedia
        let status: CallStageStatus
        /// The identity we join the SFU as.
        let own: JID

        @MainActor
        init?(state: CallState, phase: CallMediaPhase, session: SessionCoordinator) {
            let join: CallJoin
            switch state {
            case .active(let call):
                (sid, media, join, kind) = (call.sid, call.media, call.join, .direct(peer: call.peer))
                title = ConversationHeaderText.make(for: .direct(call.peer.bare), session: session).name
            case .inGroup(let call):
                guard let stage = GroupCallStage(state: state, media: phase) else { return nil }
                (sid, media, join, kind) = (call.sid, call.media, call.join, .room(stage))
                title = ConversationHeaderText.make(for: .room(call.room), session: session).title
            case .idle, .incoming, .outgoing, .joiningGroup, .ended:
                return nil
            }
            guard let own = JID(parsing: join.identity) ?? session.account.fullJID else { return nil }
            self.own = own
            status = switch phase {
            case .idle, .connecting: .connecting
            case .connected: .inCall(since: nil)
            case .reconnecting: .reconnecting
            }
        }
    }
}
