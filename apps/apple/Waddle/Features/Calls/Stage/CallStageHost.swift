import SwiftUI
import WaddleKit

/// The stage of the session's call in progress: reads the call slot, the
/// media session and the room's roster into `CallStageView`, and sends its
/// controls to the store. Draws nothing unless a call is in session.
struct CallStageHost: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(CallStageModel.self) private var model
    @Environment(CallWindowLayout.self) private var layout: CallWindowLayout?
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.callMediaControls) private var media
    @State private var speaking = SpeakingHold()
    @State private var prompts = CallStagePrompts()

    var body: some View {
        @Bindable var prompts = prompts
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
        .callSettings(isPresented: $prompts.showsSettings)
        // A list made for one call is not offered to the next.
        .onChange(of: state.sid) {
            prompts.reset()
        }
        .sheet(isPresented: Binding(get: { prompts.screenPicker != nil }, set: { if !$0 { prompts.screenPicker = nil } })) {
            if let sources = prompts.screenPicker {
                CallScreenPicker(sources: sources) { source in
                    prompts.screenPicker = nil
                    Task { [media] in await media?.shareScreen(source) }
                } cancel: {
                    prompts.screenPicker = nil
                }
            }
        }
    }

    private func stage(of call: Call) -> some View {
        let live = (media?.participants ?? []).map {
            CallStageParticipant(
                identity: $0.identity, isSelf: $0.isLocal, hasCamera: $0.isCameraOn,
                screenPublicationID: $0.screenPublicationID
            )
        }
        // The microphone starts a moment after media connects; until then it is not a mute.
        let isSelfMuted = call.status.hasMedia && media?.isMicrophoneOn == false
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
        // A full-screen call hides the pane the chat opens in: whatever the
        // inspector is set to, nobody is looking at it.
        let chat = CallStageChat(isRoom: call.isRoom, session: session, navigation: navigation, isHidden: memory.size == .immersive)
        let commands = CallStageCommands(
            sid: call.sid, session: session, media: media, stage: model,
            navigation: navigation, prompts: prompts, chat: chat
        )
        let rows = memory.showsParticipants ? CallParticipantRows.make(
            tiles: tiles,
            audio: (media?.participants ?? []).map {
                CallParticipantAudio(identity: $0.identity, hasMicrophone: $0.hasMicrophoneTrack, volume: $0.volume)
            }
        ) : nil
        let controls = CallControls(
            devices: .init(of: media, status: call.status),
            isHandRaised: call.isRoom ? session.calls.isHandRaised : nil,
            viewMode: memory.presentation.viewMode,
            isSelfViewHidden: memory.presentation.isSelfViewHidden,
            size: memory.size,
            // An open inspector has a composer whose Escape cancels a reply or an edit.
            escapeStepsDown: memory.size == .immersive || navigation.inspector == nil,
            showsParticipants: rows != nil,
            chatUnread: chat.map { $0.isOpen ? 0 : CallChat.unread(in: $0.replies, after: memory.chatSeenID) },
            showsChat: chat?.isOpen ?? false,
            canReact: call.status.hasMedia
        )
        return CallStageView(
            title: call.title,
            subtitle: subtitle,
            status: call.status,
            // The media session keeps when it connected, so the stage and a dock show one clock.
            connectedSince: media?.connectedSince,
            tiles: tiles,
            presentation: memory.presentation,
            promotedSpeaker: memory.promotedSpeaker,
            controls: controls,
            participants: rows,
            canResetVolumes: media?.hasChangedVolumes ?? false,
            participantsCoverTiles: (layout?.pane?.width ?? .infinity) < Self.widthForTilesAndPanel,
            issues: media.map { CallMediaIssues.shown(for: $0.mediaReport, dismissed: memory.dismissedMediaIssues) } ?? [],
            checkingIssues: prompts.checking,
            connection: media?.connectionQuality,
            reactions: session.calls.reactions,
            ownNick: ownNick,
            ownScreen: media?.isScreenShareOn == true ? CallStageTile(
                identity: call.own.description, source: .screen, label: "Your screen", isSelf: true, hasVideo: true
            ) : nil,
            tileSource: model.tileSource,
            perform: commands.perform
        )
        // What is in front of the person while the chat is open has been seen.
        .onChange(of: chat.flatMap { $0.isOpen ? $0.replies.last?.id : nil }, initial: true) { _, newest in
            model.update(call.sid) { $0.sawChat(upTo: newest) }
        }
        .callPushToTalk(
            hasMedia: call.status.hasMedia, isMicrophoneOn: media?.isMicrophoneOn ?? false,
            coversConversation: memory.size != .split,
            setMicrophone: CallCommands(calls: session.calls, media: media).setMicrophone
        )
        // Keyed on who has a tile, not on what the tiles show: a pin does not care who is speaking.
        .onChange(
            of: Roster(
                tiles: tiles.map(\.id), screens: tiles.map(\.screenPublicationID), isAvailable: !live.isEmpty, isSelfViewHidden: memory.presentation.isSelfViewHidden
            ),
            initial: true
        ) { _, roster in
            // During a reconnect nobody is live and the tiles are stand-ins.
            model.update(call.sid) { $0.tilesChanged(tiles, rosterIsAvailable: roster.isAvailable) }
        }
        .onChange(of: media?.activeSpeakersLoudestFirst ?? [], initial: true) { _, speakers in
            model.update(call.sid) { $0.speakersChanged(speakers) }
        }
    }

    /// A stage narrower than this gives the panel the tiles' place.
    private static let widthForTilesAndPanel: CGFloat = 720

    /// Our nick in the room whose call the slot holds.
    private var ownNick: String? {
        if case .inGroup(let call) = session.calls.state { call.nick } else { nil }
    }

    /// What the pin is reconciled against.
    private struct Roster: Equatable {
        let tiles: [CallStageTile.ID]
        /// A share that ends and starts again between two draws keeps its tile and is still a new share.
        let screens: [String?]
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
        let status: CallSessionStatus
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
            status = CallSessionStatus(phase)
        }
    }
}
