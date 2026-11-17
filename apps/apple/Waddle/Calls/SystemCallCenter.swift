#if os(iOS)
import AVFoundation
import CallKit
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
        var setMuted: @MainActor (String, Bool) async -> Void
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

    /// The call slot, its media or Do Not Disturb changed.
    func update(state: CallState, isMediaConnected: Bool, isMuted: Bool, isDoNotDisturb: Bool) {
        let updates = tracker.update(
            state: state, isMediaConnected: isMediaConnected, isMuted: isMuted, isDoNotDisturb: isDoNotDisturb)
        for update in updates {
            tell(update)
        }
        audio.setOwner(tracker.audioOwner)
    }

    private func tell(_ update: SystemCallUpdate) {
        switch update {
        case .reportIncoming(let call):
            provider.reportNewIncomingCall(with: call.id, update: describe(call)) { [weak self] error in
                Task { @MainActor in self?.settled(call, error: error) }
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

    /// Takes a call the system knows off its screen and leaves it to the app.
    private func abandon(_ id: UUID) {
        guard tracker.managedCall?.id == id else { return }
        refused(id)
        provider.reportCall(with: id, endedAt: nil, reason: .failed)
    }

    private func handle(for call: SystemCall) -> CXHandle {
        CXHandle(type: .generic, value: call.conversation.jid.description)
    }

    private func describe(_ call: SystemCall) -> CXCallUpdate {
        let update = CXCallUpdate()
        update.remoteHandle = handle(for: call)
        update.localizedCallerName = handlers?.name(call.conversation)
        update.hasVideo = call.isVideo
        update.supportsHolding = false
        update.supportsGrouping = false
        update.supportsUngrouping = false
        update.supportsDTMF = false
        return update
    }

    private static func reason(_ end: SystemCallEnd) -> CXCallEndedReason {
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
            tracker.systemEnded(call.id)
            audio.setOwner(tracker.audioOwner)
            Task { await handlers?.end(call.sid) }
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
            Task {
                if await handlers.answer(sid) {
                    tracker.systemAnswered(id)
                    action.fulfill()
                } else {
                    // The system takes a failed call off its screen; the
                    // slot may still ring, and is the app's alone now.
                    refused(id)
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
                tracker.systemEnded(action.callUUID)
                audio.setOwner(tracker.audioOwner)
                Task { await handlers.end(sid) }
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
