import Foundation
import Observation
import WaddleKit

/// What a stage is showing over itself for the call it was drawn for: a screen picker,
/// the settings sheet, the devices being checked again. Per window, held by the host.
@MainActor @Observable
final class CallStagePrompts {
    var screenPicker: CallScreenSources?
    var showsSettings = false
    private var checks: [CallMediaIssue.Kind: UUID] = [:]
    var checking: Set<CallMediaIssue.Kind> { Set(checks.keys) }

    func reset() {
        screenPicker = nil
        showsSettings = false
        checks = [:]
    }

    func startChecking(_ kind: CallMediaIssue.Kind) -> UUID {
        let check = UUID()
        checks[kind] = check
        return check
    }

    func finishChecking(_ kind: CallMediaIssue.Kind, check: UUID) {
        if checks[kind] == check { checks[kind] = nil }
    }
}

/// The chat of a room's call: the thread its announcement heads, with
/// the messages of it that are loaded. A call of two has none here yet.
struct CallStageChat {
    let conversation: ConversationID
    let thread: String
    let replies: [TimelineItem]
    /// The thread is the one open beside the conversation.
    let isOpen: Bool

    @MainActor
    init?(isRoom: Bool, session: SessionCoordinator, navigation: NavigationModel, isHidden: Bool) {
        guard isRoom, case .inGroup(let call) = session.calls.state else { return nil }
        let conversation = ConversationID.room(call.room)
        let timeline = session.timelines.timeline(for: conversation)
        guard let thread = CallChat.thread(in: timeline.items) else { return nil }
        self.conversation = conversation
        self.thread = thread
        replies = timeline.threadReplies(threadID: thread)
        isOpen = !isHidden && navigation.selection == conversation && navigation.inspector == .thread(rootID: thread)
    }
}

/// Performs what is asked of the stage of one call. Bound to that call's session id when it is made.
@MainActor
struct CallStageCommands {
    private let sid: String
    private let calls: CallStore
    private let media: (any CallMediaControls)?
    private let stage: CallStageModel
    private let navigation: NavigationModel
    private let prompts: CallStagePrompts
    private let chat: CallStageChat?
    private let commands: CallCommands

    init(
        sid: String, session: SessionCoordinator, media: (any CallMediaControls)?, stage: CallStageModel,
        navigation: NavigationModel, prompts: CallStagePrompts, chat: CallStageChat?
    ) {
        self.sid = sid
        calls = session.calls
        self.media = media
        self.stage = stage
        self.navigation = navigation
        self.prompts = prompts
        self.chat = chat
        commands = CallCommands(calls: session.calls, media: media)
    }

    func perform(_ action: CallStageAction) {
        switch action {
        case .toggleMicrophone: commands.toggleMicrophone()
        case .toggleCamera: commands.toggleCamera()
        case .toggleHand: commands.toggleHand()
        case .toggleScreenShare: toggleScreenShare()
        case .toggleChat: toggleChat()
        case .toggleParticipants: toggleParticipants()
        case .toggleSelfView: toggleSelfView()
        case .togglePictureInPicture: stage.update(sid) { $0.toggleFloating() }
        case .minimise: stage.update(sid) { $0.minimise() }
        case .openSettings: openSettings()
        case .resetVolumes: resetVolumes()
        case .hangUp: commands.hangUp()
        case .setViewMode(let mode): setViewMode(mode)
        case .setSize(let size): setSize(size)
        case .setVolume(let identity, let gain): setVolume(identity: identity, gain: gain)
        case .react(let emoji): react(emoji)
        case .retry(let kind): retry(kind)
        case .dismissIssue(let kind): dismissIssue(kind)
        case .openSystemSettings(let kind): CallSystemSettings.open(for: kind)
        }
    }

    /// The slot still holds the call this stage was drawn for.
    private var isThisCall: Bool { calls.state.sid == sid }

    private func setViewMode(_ mode: CallStagePresentation.ViewMode) {
        stage.update(sid) { $0.presentation.viewMode = mode }
    }

    private func toggleSelfView() {
        stage.update(sid) { $0.presentation.isSelfViewHidden.toggle() }
    }

    private func setSize(_ size: CallSurface.Size) {
        stage.update(sid) { $0.size = size }
    }

    private func toggleParticipants() {
        stage.update(sid) { $0.toggleParticipants() }
    }

    private func setVolume(identity: String, gain: Double) {
        media?.setVolume(gain, of: identity)
    }

    private func resetVolumes() {
        media?.resetVolumes()
    }

    private func openSettings() {
        prompts.showsSettings = true
    }

    private func react(_ emoji: String) {
        // For the call the stage was drawn for, while it is still that call.
        guard isThisCall else { return }
        Task {
            guard isThisCall else { return }
            await calls.sendReaction(emoji)
        }
    }

    private func toggleChat() {
        guard let chat else { return }
        if chat.isOpen {
            navigation.inspector = nil
            return
        }
        stage.update(sid) {
            // The chat opens beside the pane the stage is in; only a
            // full-screen call, which hides everything else, is in its way.
            $0.size = $0.size.afterFullscreenExit
            $0.sawChat(upTo: chat.replies.last?.id)
        }
        navigation.openThread(chat.thread, in: chat.conversation, usesInspector: true)
    }

    private func retry(_ kind: CallMediaIssue.Kind) {
        // For the call the notice is about, while it is still a call.
        guard let media, !prompts.checking.contains(kind), calls.state.sid == sid,
              calls.state.isEstablished else { return }
        // Before the task: two taps in a row are one check.
        let check = prompts.startChecking(kind)
        Task {
            defer { prompts.finishChecking(kind, check: check) }
            // The call can end, and another begin, before this runs.
            guard isThisCall, calls.state.isEstablished else { return }
            switch kind {
            case .microphone: await media.setMicrophone(true)
            case .camera: await media.setCamera(true)
            case .background: await media.restartBackground()
            case .screen:
                let sources = await media.screenSources()
                if calls.state.sid == sid { prompts.screenPicker = sources }
            }
            // The system answers a refusal at once: the button would
            // flicker and look as if nothing had been asked.
            try? await Task.sleep(for: .milliseconds(600))
        }
    }

    // Put away until its device is refused once more, whichever surface asks (`CallMediaIssues.Dismissals`).
    private func dismissIssue(_ kind: CallMediaIssue.Kind) {
        guard let report = media?.mediaReport else { return }
        stage.update(sid) { $0.dismissedMediaIssues.dismiss(kind, in: report) }
    }

    private func toggleScreenShare() {
        guard let media, isThisCall else { return }
        Task {
            // A share that is on by the time this runs may be the next call's.
            guard isThisCall else { return }
            if media.isSharingOrStartingScreen {
                await media.stopScreenShare()
            } else {
                // Refused: the stage's notice says so and there is nothing to pick.
                let sources = await media.screenSources()
                // The list is for the call it was asked from.
                if calls.state.sid == sid { prompts.screenPicker = sources }
            }
        }
    }
}
