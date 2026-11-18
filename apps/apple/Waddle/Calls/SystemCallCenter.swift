#if os(iOS)
import AVFoundation
import CallKit
import Foundation
import WaddleKit

/// Calls on an iPhone or iPad, as the system knows them: the call screen
/// that rings, the green pill, the lock screen, CarPlay.
///
/// `SystemCallTracker` decides what the system is told about the call
/// slot; this tells it, and carries what the person does on the system's
/// screen back to the slot. Audio follows the tracker's `audioOwner`
/// (`SystemCallAudio`): the system owns the session of a call it manages.
@MainActor
final class SystemCallCenter: NSObject {
    /// What the system's screen can ask of the call with a session id.
    struct Handlers {
        /// Answers the call; true when its answer has gone out.
        var answer: @MainActor (String) async -> Bool
        var end: @MainActor (String) async -> Void
        /// Records the ending before any task can restore or adopt the call.
        var finish: @MainActor (String, Bool) -> Void
        /// Releases push metadata after an answer action fails.
        var expirePush: @MainActor (String) -> Void
        var setMuted: @MainActor (String, Bool) async -> Void
        var isReady: @MainActor (String) -> Bool
        var state: @MainActor () -> CallState
        var isFinished: @MainActor (String) -> Bool
        /// The name to show for a person or a room.
        var name: @MainActor (ConversationID) -> String
    }

    var handlers: Handlers?

    private let provider: CXProvider
    private let controller = CXCallController()
    private let audio = SystemCallAudio()
    private var tracker = SystemCallTracker()

    override init() {
        let configuration = CXProviderConfiguration()
        configuration.supportsVideo = true
        configuration.maximumCallGroups = 1
        configuration.maximumCallsPerCallGroup = 1
        configuration.supportedHandleTypes = [.generic]
        // Recents sync to iCloud; who called whom stays in Waddle.
        configuration.includesCallsInRecents = false
        provider = CXProvider(configuration: configuration)
        super.init()
        provider.setDelegate(self, queue: .main)
    }

    /// Every push is reported before restoration or any other asynchronous work.
    @discardableResult
    func receive(_ push: CallPush?, callerName: String, completion: @escaping @MainActor () -> Void) -> PushedCall.Verdict {
        guard let push else {
            let id = UUID()
            let update = CXCallUpdate()
            update.localizedCallerName = callerName
            provider.reportNewIncomingCall(with: id, update: update) { [weak self] _ in
                // The queue of a report's completion is not documented.
                Task { @MainActor in
                    self?.provider.reportCall(with: id, endedAt: nil, reason: .unanswered)
                    completion()
                }
            }
            return .dismissed
        }
        let result = tracker.pushed(
            sid: push.sid, conversation: push.conversation, isVideo: push.isVideo,
            isExpired: push.isExpired(at: .now), isFinished: handlers?.isFinished(push.sid) ?? false,
            state: handlers?.state() ?? .idle)
        for update in result.updates { tell(update, callerName: callerName, completion: completion) }
        audio.setOwner(tracker.audioOwner)
        return result.verdict
    }

    /// True while a system answer still needs the pushed call's owner.
    @discardableResult
    func pushExpired(sid: String, cancellingAnswer: Bool = false) -> Bool {
        if cancellingAnswer, tracker.isAwaitingSlot, let call = tracker.managedCall, call.sid == sid {
            tracker.systemAnswerFailed(call.id)
        }
        for update in tracker.pushExpired(sid: sid) { tell(update) }
        audio.setOwner(tracker.audioOwner)
        return tracker.isAwaitingSlot && tracker.managedCall?.sid == sid
    }

    func sessionEnded() {
        for update in tracker.sessionEnded() { tell(update) }
        audio.setOwner(tracker.audioOwner)
    }

    /// The directory can name a call after the push has already reported it.
    func refreshName() {
        guard let call = tracker.managedCall else { return }
        provider.reportCall(with: call.id, updated: describe(call))
    }

    /// The call slot, its media or Do Not Disturb changed.
    func update(state: CallState, isMediaConnected: Bool, isMuted: Bool, isDoNotDisturb: Bool) {
        let updates = tracker.update(
            state: state, isMediaConnected: isMediaConnected, isMuted: isMuted, isDoNotDisturb: isDoNotDisturb)
        for update in updates {
            tell(update)
        }
        audio.setOwner(tracker.audioOwner)
    }

    private func tell(_ update: SystemCallUpdate, callerName: String? = nil, completion: @escaping @MainActor () -> Void = {}) {
        switch update {
        case .reportIncoming(let call):
            provider.reportNewIncomingCall(with: call.id, update: describe(call, callerName: callerName)) { [weak self] error in
                Task { @MainActor in
                    self?.settled(call, error: error)
                    completion()
                }
            }
        case .reportDuplicate(let call):
            // A duplicate-report error says nothing about the original report.
            provider.reportNewIncomingCall(with: call.id, update: describe(call, callerName: callerName)) { _ in
                Task { @MainActor in completion() }
            }
        case .reportDismissed(let call, let end):
            provider.reportNewIncomingCall(with: call.id, update: describe(call, callerName: callerName)) { [weak self] _ in
                Task { @MainActor in
                    self?.provider.reportCall(with: call.id, endedAt: nil, reason: Self.reason(end))
                    completion()
                }
            }
        case .startOutgoing(let call):
            let action = CXStartCallAction(call: call.id, handle: handle(for: call))
            action.isVideo = call.isVideo
            controller.request(CXTransaction(action: action)) { [weak self] error in
                Task { @MainActor in
                    guard let self else { return }
                    self.settled(call, error: error)
                    if error == nil { self.provider.reportCall(with: call.id, updated: self.describe(call)) }
                }
            }
        case .answer(let id):
            controller.request(CXTransaction(action: CXAnswerCallAction(call: id))) { [weak self] error in
                // The system could not show it answered, so it will not
                // hand over audio either: the call is the app's alone.
                guard error != nil else { return }
                Task { @MainActor in self?.abandon(id) }
            }
        case .connecting(let id):
            provider.reportOutgoingCall(with: id, startedConnectingAt: nil)
        case .connected(let id):
            provider.reportOutgoingCall(with: id, connectedAt: nil)
        case .setMuted(let id, let isMuted):
            // Muted in the dock: the system's mute button follows. The
            // action comes back through the delegate, which sets the
            // microphone to what it already is.
            controller.request(CXTransaction(action: CXSetMutedCallAction(call: id, muted: isMuted))) { _ in }
        case .end(let id, let reason):
            provider.reportCall(with: id, endedAt: nil, reason: Self.reason(reason))
        }
    }

    /// The system answered a report or a start. A Focus, a blocked caller
    /// or a busy line makes it refuse, and the call is the app's alone. If
    /// the call ended while the system was still taking it, the end it was
    /// told then was for a call it did not know yet, so it is told again.
    private func settled(_ call: SystemCall, error: Error?) {
        if error != nil {
            refused(call.id)
        } else if tracker.managedCall?.id != call.id {
            provider.reportCall(with: call.id, endedAt: nil, reason: .failed)
        }
    }

    private func refused(_ id: UUID) {
        tracker.systemRefused(id)
        audio.setOwner(tracker.audioOwner)
    }

    private func refuseAnswer(_ id: UUID, sid: String) {
        refused(id)
        // The push deadline may have passed while answering. No later
        // deadline remains to clear an awaiting refusal or its owner.
        pushExpired(sid: sid)
        handlers?.expirePush(sid)
    }

    /// Takes a call the system knows off its screen and leaves it to the app.
    private func abandon(_ id: UUID) {
        guard tracker.managedCall?.id == id else { return }
        refused(id)
        provider.reportCall(with: id, endedAt: nil, reason: .failed)
    }

    private func handle(for call: SystemCall) -> CXHandle {
        CXHandle(type: .generic, value: call.conversation.jid.description)
    }

    private func describe(_ call: SystemCall, callerName: String? = nil) -> CXCallUpdate {
        let update = CXCallUpdate()
        update.remoteHandle = handle(for: call)
        update.localizedCallerName = callerName ?? handlers?.name(call.conversation)
            ?? DirectoryStore().title(for: call.conversation)
        update.hasVideo = call.isVideo
        update.supportsHolding = false
        update.supportsGrouping = false
        update.supportsUngrouping = false
        update.supportsDTMF = false
        return update
    }

    nonisolated private static func reason(_ end: SystemCallEnd) -> CXCallEndedReason {
        switch end {
        case .remoteEnded: .remoteEnded
        case .unanswered: .unanswered
        case .answeredElsewhere: .answeredElsewhere
        case .failed: .failed
        }
    }
}

/// The delegate is called on the main queue (`setDelegate(_:queue:)`).
extension SystemCallCenter: CXProviderDelegate {
    nonisolated func providerDidReset(_ provider: CXProvider) {
        MainActor.assumeIsolated {
            // The system dropped every call it knew; ours ends with them.
            audio.deactivated()
            guard let call = tracker.managedCall else { return }
            let awaiting = tracker.isAwaitingSlot && !(handlers?.state().sid == call.sid && handlers?.state().isLive == true)
            tracker.systemEnded(call.id)
            if awaiting {
                // A provider reset is not the person's decline.
                handlers?.expirePush(call.sid)
            } else {
                handlers?.finish(call.sid, false)
                Task { await handlers?.end(call.sid) }
            }
            audio.setOwner(tracker.audioOwner)
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
        // The call was already placed when the system heard of it.
        action.fulfill()
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
        MainActor.assumeIsolated {
            let id = action.callUUID
            guard let sid = tracker.sid(for: id), let handlers else { return action.fail() }
            tracker.systemAnswering(id)
            Task {
                // A transport send need not honour cancellation. The action's
                // deadline still takes its screen and audio ownership back.
                var settled = false
                let deadline = Task { @MainActor in
                    do { try await Task.sleep(for: .seconds(max(0, action.timeoutDate.timeIntervalSinceNow))) }
                    catch { return }
                    guard !settled else { return }
                    settled = true
                    refuseAnswer(id, sid: sid)
                    action.fail()
                }
                defer {
                    deadline.cancel()
                    tracker.systemAnswerFailed(id)
                }
                while tracker.sid(for: id) == sid && !handlers.isReady(sid) && !settled {
                    do { try await Task.sleep(for: .milliseconds(50)) } catch { break }
                }
                guard !settled else { return }
                guard tracker.sid(for: id) == sid, Date.now < action.timeoutDate, handlers.isReady(sid) else {
                    settled = true
                    refuseAnswer(id, sid: sid)
                    action.fail()
                    return
                }
                let answered = await handlers.answer(sid)
                guard !settled else { return }
                settled = true
                if answered, tracker.sid(for: id) == sid, Date.now < action.timeoutDate {
                    tracker.systemAnswered(id)
                    action.fulfill()
                } else {
                    // The slot may still ring, and is the app's alone now.
                    refuseAnswer(id, sid: sid)
                    action.fail()
                }
            }
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        MainActor.assumeIsolated {
            // Over for the system at once: telling the other side can take
            // as long as the network does.
            if let sid = tracker.sid(for: action.callUUID), let handlers {
                let awaiting = tracker.isAwaitingSlot && !(handlers.state().sid == sid && handlers.state().isLive)
                tracker.systemEnded(action.callUUID)
                handlers.finish(sid, awaiting)
                audio.setOwner(tracker.audioOwner)
                if !awaiting { Task { await handlers.end(sid) } }
            }
            action.fulfill()
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
        MainActor.assumeIsolated {
            guard let sid = tracker.sid(for: action.callUUID), let handlers else { return action.fail() }
            let isMuted = action.isMuted
            tracker.systemSetMuted(action.callUUID, isMuted)
            Task {
                await handlers.setMuted(sid, isMuted)
                action.fulfill()
            }
        }
    }

    nonisolated func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated { audio.activated(audioSession) }
    }

    nonisolated func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated { audio.deactivated() }
    }
}
#endif
