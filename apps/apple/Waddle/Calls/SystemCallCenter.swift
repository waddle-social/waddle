#if os(iOS)
import AVFoundation
import CallKit
import WaddleKit

/// Calls on an iPhone or iPad, as the system knows them: the call screen
/// that rings, the green pill, the lock screen, CarPlay.
///
/// `SystemCallTracker` decides what the system is told about the call
/// slot; this tells it, and carries what the person does on the system's
/// screen back to the slot. Audio follows (`SystemCallAudio`): the system
/// owns the session of a call it manages.
@MainActor
final class SystemCallCenter: NSObject {
    /// What the system's screen can ask of the call with a session id.
    struct Handlers {
        /// Answers the call; true when it is answered, by this or already.
        var answer: @MainActor (String) async -> Bool
        var end: @MainActor (String) async -> Void
        var setMuted: @MainActor (String, Bool) async -> Void
        /// The name to show for a person or a room.
        var name: @MainActor (SystemCall) -> String
    }

    var handlers: Handlers?

    private let provider: CXProvider
    private let controller = CXCallController()
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
    func update(state: CallState, isMediaConnected: Bool, isDoNotDisturb: Bool) {
        for update in tracker.update(state: state, isMediaConnected: isMediaConnected, isDoNotDisturb: isDoNotDisturb) {
            tell(update)
        }
        SystemCallAudio.setManaged(tracker.isManaging)
    }

    private func tell(_ update: SystemCallUpdate) {
        switch update {
        case .reportIncoming(let call):
            provider.reportNewIncomingCall(with: call.id, update: describe(call)) { [weak self] error in
                // A Focus, a blocked caller or a busy line: the system will
                // not ring, and the call is the app's alone.
                guard error != nil else { return }
                Task { @MainActor in self?.refused(call.id) }
            }
        case .startOutgoing(let call):
            let action = CXStartCallAction(call: call.id, handle: handle(for: call))
            action.isVideo = call.isVideo
            controller.request(CXTransaction(action: action)) { [weak self] error in
                Task { @MainActor in
                    guard let self else { return }
                    if error != nil { self.refused(call.id) } else { self.provider.reportCall(with: call.id, updated: self.describe(call)) }
                }
            }
        case .answer(let id):
            controller.request(CXTransaction(action: CXAnswerCallAction(call: id))) { _ in }
        case .connecting(let id):
            provider.reportOutgoingCall(with: id, startedConnectingAt: nil)
        case .connected(let id):
            provider.reportOutgoingCall(with: id, connectedAt: nil)
        case .end(let id, let reason):
            provider.reportCall(with: id, endedAt: nil, reason: Self.reason(reason))
        }
    }

    private func refused(_ id: UUID) {
        tracker.systemRefused(id)
        SystemCallAudio.setManaged(tracker.isManaging)
    }

    private func handle(for call: SystemCall) -> CXHandle {
        CXHandle(type: .generic, value: call.peer.description)
    }

    private func describe(_ call: SystemCall) -> CXCallUpdate {
        let update = CXCallUpdate()
        update.remoteHandle = handle(for: call)
        update.localizedCallerName = handlers?.name(call)
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
            SystemCallAudio.deactivated()
            guard let call = tracker.managedCall else { return }
            tracker.systemEnded(call.id)
            SystemCallAudio.setManaged(false)
            Task { await handlers?.end(call.sid) }
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
        // The call was already placed when the system heard of it.
        action.fulfill()
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
        MainActor.assumeIsolated {
            guard let sid = tracker.sid(for: action.callUUID), let handlers else { return action.fail() }
            tracker.systemAnswered(action.callUUID)
            Task { await handlers.answer(sid) ? action.fulfill() : action.fail() }
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        MainActor.assumeIsolated {
            guard let sid = tracker.sid(for: action.callUUID), let handlers else { return action.fulfill() }
            tracker.systemEnded(action.callUUID)
            Task {
                await handlers.end(sid)
                action.fulfill()
            }
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
        MainActor.assumeIsolated {
            guard let sid = tracker.sid(for: action.callUUID), let handlers else { return action.fail() }
            let isMuted = action.isMuted
            Task {
                await handlers.setMuted(sid, isMuted)
                action.fulfill()
            }
        }
    }

    nonisolated func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated { SystemCallAudio.activated(audioSession) }
    }

    nonisolated func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated { SystemCallAudio.deactivated() }
    }
}
#endif
