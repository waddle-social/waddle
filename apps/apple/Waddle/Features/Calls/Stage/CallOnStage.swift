import WaddleKit

/// The slot's call in session, as whatever draws its tiles reads it: the
/// stage, and the window a call floats in.
@MainActor
struct CallOnStage {
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
    let status: CallSessionStatus
    /// The identity we join the SFU as.
    let own: JID

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
        status = CallSessionStatus(phase)
    }

    /// Who has a tile, and the line under the call's name.
    struct Cast {
        let tiles: [CallStageTile]
        let subtitle: String
        /// The media session reports who is there; during a reconnect nobody is live and the tiles are stand-ins.
        let isRosterAvailable: Bool
    }

    /// - Parameter speaking: the accounts shown as speaking.
    func cast(media controls: (any CallMediaControls)?, session: SessionCoordinator, speaking: Set<BareJID>) -> Cast {
        let live = (controls?.participants ?? []).map {
            CallStageParticipant(
                identity: $0.identity, isSelf: $0.isLocal, hasCamera: $0.isCameraOn,
                screenPublicationID: $0.screenPublicationID
            )
        }
        // The microphone starts a moment after media connects; until then it is not a mute.
        let isSelfMuted = status.hasMedia && controls?.isMicrophoneOn == false
        switch kind {
        case .direct(let peer):
            return Cast(
                tiles: CallStageTiles.direct(
                    live: live, own: own, peer: peer, peerName: title, isSelfMuted: isSelfMuted, speaking: speaking
                ),
                subtitle: CallStageSubtitle.direct(media), isRosterAvailable: !live.isEmpty
            )
        case .room(let stage):
            let rows = session.callRoster(of: stage, media: controls, speaking: speaking)
            return Cast(
                tiles: CallStageTiles.room(live: live, own: own, rows: rows, isSelfMuted: isSelfMuted),
                subtitle: CallStageSubtitle.room(others: rows.count { !$0.isSelf }), isRosterAvailable: !live.isEmpty
            )
        }
    }
}
