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
    @State private var chrome = CallChromeFader()
    /// The stage covers a phone's screen (`CallSurface.cover`): it has one
    /// size, a way back to the dock in place of the steps, and its
    /// participants list is a sheet.
    var coversScreen = false

    var body: some View {
        @Bindable var prompts = prompts
        let state = session.calls.state
        let speakers = (media?.participants ?? []).lazy.filter(\.isSpeaking).map(\.identity)
        Group {
            if let call = CallOnStage(state: state, phase: media?.phase ?? .idle, session: session) {
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

    private func stage(of call: CallOnStage) -> some View {
        let cast = call.cast(media: media, session: session, speaking: speaking.shown)
        let tiles = cast.tiles
        let memory = model.stage(of: call.sid)
        // Over a phone's screen the stage is as large as it gets, with its header and controls in place.
        let size = coversScreen ? .expanded : memory.size
        // A full-screen call hides the pane the chat opens in: whatever the
        // inspector is set to, nobody is looking at it. A phone has no pane
        // beside the stage for the chat at all.
        let chat = coversScreen ? nil : CallStageChat(
            isRoom: call.isRoom, session: session, navigation: navigation, isHidden: size == .immersive
        )
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
            size: size,
            // An open inspector has a composer whose Escape cancels a reply or an edit.
            escapeStepsDown: !coversScreen && (size == .immersive || navigation.inspector == nil),
            showsParticipants: rows != nil,
            chatUnread: chat.map { $0.isOpen ? 0 : CallChat.unread(in: $0.replies, after: memory.chatSeenID) },
            showsChat: chat?.isOpen ?? false,
            canReact: call.status.hasMedia,
            isFloating: isFloating(memory, tiles: tiles),
            minimises: coversScreen
        )
        return CallStageView(
            title: call.title,
            subtitle: cast.subtitle,
            status: call.status,
            // The media session keeps when it connected, so the stage and a dock show one clock.
            connectedSince: media?.connectedSince,
            tiles: tiles,
            presentation: memory.presentation,
            promotedSpeaker: memory.promotedSpeaker,
            controls: controls,
            // A phone has no room for the list beside the tiles: it is a sheet over them.
            participants: coversScreen ? nil : rows,
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
            isChromeHidden: size == .immersive && !chrome.isShown,
            tileSource: model.tileSource,
            // Using the call, by a key too, is a sign of life as much as a move of the pointer.
            perform: { [chrome] action in
                chrome.stir()
                commands.perform(action)
            }
        )
        .callChromeFade(
            chrome, isOn: size == .immersive,
            isHeld: rows != nil || prompts.showsSettings || prompts.screenPicker != nil
        )
        .sheet(isPresented: Binding(
            get: { coversScreen && rows != nil },
            set: { shows in if !shows { model.update(call.sid) { if $0.showsParticipants { $0.toggleParticipants() } } } }
        )) {
            CallParticipantsSheet(rows: rows ?? [], canResetVolumes: media?.hasChangedVolumes ?? false, perform: commands.perform)
        }
        // What is in front of the person while the chat is open has been seen.
        .onChange(of: chat.flatMap { $0.isOpen ? $0.replies.last?.id : nil }, initial: true) { _, newest in
            model.update(call.sid) { $0.sawChat(upTo: newest) }
        }
        .callPushToTalk(
            hasMedia: call.status.hasMedia, isMicrophoneOn: media?.isMicrophoneOn ?? false,
            coversConversation: size != .split,
            setMicrophone: CallCommands(calls: session.calls, media: media).setMicrophone
        )
        // Keyed on who has a tile, not on what the tiles show: a pin does not care who is speaking.
        .onChange(
            of: Roster(
                tiles: tiles.map(\.id), screens: tiles.map(\.screenPublicationID), isAvailable: cast.isRosterAvailable, isSelfViewHidden: memory.presentation.isSelfViewHidden
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

    /// Whether the call floats; `nil` while nobody has a picture to float,
    /// and on iPad, where the system's picture-in-picture is yet to come (#2697).
    private func isFloating(_ memory: CallStageMemory, tiles: [CallStageTile]) -> Bool? {
        #if os(macOS)
        if memory.isFloating { return true }
        let floats = CallPictureInPicture.tile(in: tiles, presentation: memory.presentation, speaker: memory.promotedSpeaker)
        return floats == nil ? nil : false
        #else
        nil
        #endif
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
}
