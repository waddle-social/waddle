import Foundation

/// A call as the operating system's call screen knows it.
public struct SystemCall: Hashable, Sendable {
    public enum Direction: Hashable, Sendable { case incoming, outgoing }

    /// The system's name for the call; ours is `sid`.
    public let id: UUID
    public let sid: String
    public let direction: Direction
    /// The person, or the room whose call it is.
    public let conversation: ConversationID
    public let isVideo: Bool

    public init(id: UUID, sid: String, direction: Direction, conversation: ConversationID, isVideo: Bool) {
        self.id = id
        self.sid = sid
        self.direction = direction
        self.conversation = conversation
        self.isVideo = isVideo
    }
}

/// Who may run the microphone and speaker for the slot's call.
public enum SystemCallAudioOwner: Hashable, Sendable {
    /// The system's call screen manages the call and its audio session.
    case system
    /// The call is live but the system is not told of it.
    case app
    /// No call, or one that is over as far as audio goes.
    case nobody
}

/// Why a call left the system's call screen without the person ending it there.
public enum SystemCallEnd: Hashable, Sendable {
    case remoteEnded, unanswered, answeredElsewhere, failed
}

/// What to tell the system about the call slot.
public enum SystemCallUpdate: Hashable, Sendable {
    case reportIncoming(SystemCall)
    /// Re-report the same call for another push; ignore the report's answer.
    case reportDuplicate(SystemCall)
    /// Report a fresh call, then end it from the report's completion.
    case reportDismissed(SystemCall, SystemCallEnd)
    case startOutgoing(SystemCall)
    /// The call was answered in the app, not on the system's screen.
    case answer(UUID)
    /// The other side picked up, or the room's mixer let us in.
    case connecting(UUID)
    case connected(UUID)
    /// The microphone was muted or unmuted in the app.
    case setMuted(UUID, Bool)
    case end(UUID, SystemCallEnd)
}

/// Every VoIP push needs a system report, even when its call cannot ring.
public struct PushedCall: Hashable, Sendable {
    public enum Verdict: Hashable, Sendable { case awaiting, duplicate, dismissed }

    public let verdict: Verdict
    public let updates: [SystemCallUpdate]
}

/// Keeps the system's call screen in step with the call slot, telling it
/// each thing once.
///
/// One call at a time, as the slot holds. A call is *managed* from the
/// moment it is reported until it ends or the system refuses it, and the
/// system owns audio only for a managed call (`audioOwner`).
///
/// Not every call is reported. One that arrives under Do Not Disturb stays
/// off the system's screen for good, as does one first seen already
/// answered, and one the system refused: those are left to the app, which
/// shows them in its dock without a ring.
public struct SystemCallTracker {
    private enum Progress: Comparable { case started, connecting, connected }

    private struct Managed {
        var call: SystemCall
        var isAnswerKnown = false
        var progress = Progress.started
        /// What the system's screen shows for the microphone.
        var isMuted = false
        var isAwaitingSlot = false
    }

    private let newID: () -> UUID
    private var managed: Managed?
    /// The slot's call that the system is not told about.
    private var unmanagedSid: String?
    /// A refused or ended push may still be waiting for its proposal.
    private var unmanagedIsAwaitingSlot = false
    /// The slot's call that ended on the system's side; the slot may hold
    /// it a moment longer while it is torn down.
    private var endedSid: String?

    public init(newID: @escaping () -> UUID = { UUID() }) {
        self.newID = newID
    }

    /// The system knows about the slot's call.
    public var isManaging: Bool { managed != nil }
    public var managedCall: SystemCall? { managed?.call }
    public var isAwaitingSlot: Bool { managed?.isAwaitingSlot == true }

    /// Reports a push before its proposal reaches the slot. A duplicate
    /// uses the known id; a call that cannot ring gets a disposable one.
    public mutating func pushed(
        sid: String, conversation: BareJID, isVideo: Bool, isExpired: Bool, isFinished: Bool, state: CallState
    ) -> PushedCall {
        if let current = managed, current.call.sid == sid {
            return PushedCall(verdict: .duplicate, updates: [.reportDuplicate(current.call)])
        }
        let call = SystemCall(id: newID(), sid: sid, direction: .incoming, conversation: .direct(conversation), isVideo: isVideo)
        guard managed == nil, !state.isLive, !isExpired, !isFinished,
            state.sid != sid, unmanagedSid != sid, endedSid != sid
        else {
            return PushedCall(verdict: .dismissed, updates: [.reportDismissed(call, .unanswered)])
        }
        managed = Managed(call: call, isAwaitingSlot: true)
        return PushedCall(verdict: .awaiting, updates: [.reportIncoming(call)])
    }

    /// The push's deadline bounds only the wait for its proposal.
    public mutating func pushExpired(sid: String) -> [SystemCallUpdate] {
        guard let current = managed, current.isAwaitingSlot, current.call.sid == sid else { return [] }
        managed = nil
        return [.end(current.call.id, .unanswered)]
    }

    /// A call waiting for a session cannot survive that session going away.
    public mutating func sessionEnded() -> [SystemCallUpdate] {
        guard let current = managed, current.isAwaitingSlot else { return [] }
        managed = nil
        return [.end(current.call.id, .failed)]
    }

    public var audioOwner: SystemCallAudioOwner {
        if managed != nil { return .system }
        return unmanagedSid != nil && unmanagedSid != endedSid ? .app : .nobody
    }

    /// Our session id for the system's call, while it lasts.
    public func sid(for id: UUID) -> String? {
        managed?.call.id == id ? managed?.call.sid : nil
    }

    /// The system knows the call is answered: the person answered on its
    /// screen and the answer went out.
    public mutating func systemAnswered(_ id: UUID) {
        guard managed?.call.id == id else { return }
        managed?.isAnswerKnown = true
    }

    /// The person muted or unmuted on the system's screen, so it need not
    /// be told.
    public mutating func systemSetMuted(_ id: UUID, _ muted: Bool) {
        guard managed?.call.id == id else { return }
        managed?.isMuted = muted
    }

    /// The call is over on the system's side: the person ended it there,
    /// or the system dropped it. It need not be told, and the call's audio
    /// is over even while the slot still holds it.
    public mutating func systemEnded(_ id: UUID) {
        guard let call = managed?.call, call.id == id else { return }
        unmanagedIsAwaitingSlot = isAwaitingSlot
        managed = nil
        unmanagedSid = call.sid
        endedSid = call.sid
    }

    /// The system would not take the call, or lost track of it: it lives
    /// on as the app's alone.
    public mutating func systemRefused(_ id: UUID) {
        guard let call = managed?.call, call.id == id else { return }
        unmanagedIsAwaitingSlot = isAwaitingSlot
        managed = nil
        unmanagedSid = call.sid
    }

    /// - Parameter isMuted: the person chose to mute the microphone.
    public mutating func update(
        state: CallState, isMediaConnected: Bool, isMuted: Bool = false, isDoNotDisturb: Bool = false
    ) -> [SystemCallUpdate] {
        var updates: [SystemCallUpdate] = []
        let live = state.isLive ? state.sid : nil
        carryOverReplacedSession(to: state)
        if let current = managed, current.isAwaitingSlot {
            if live == nil { return [] }
            if live != current.call.sid {
                updates.append(.end(current.call.id, .unanswered))
                managed = nil
            } else {
                managed?.isAwaitingSlot = false
                if state.isIncoming, isDoNotDisturb {
                    updates.append(.end(current.call.id, .unanswered))
                    managed = nil
                    unmanagedSid = current.call.sid
                }
            }
        }
        if let current = managed, current.call.sid != live {
            updates.append(.end(current.call.id, Self.end(of: state, for: current.call)))
            managed = nil
        }
        if unmanagedSid == live {
            unmanagedIsAwaitingSlot = false
        } else if live != nil || !unmanagedIsAwaitingSlot {
            unmanagedSid = nil
            endedSid = nil
            unmanagedIsAwaitingSlot = false
        }
        guard let sid = live, unmanagedSid == nil else { return updates }
        if managed == nil {
            guard let call = newCall(for: state, sid: sid, isDoNotDisturb: isDoNotDisturb) else {
                unmanagedSid = sid
                return updates
            }
            managed = Managed(call: call)
            updates.append(call.direction == .incoming ? .reportIncoming(call) : .startOutgoing(call))
        }
        updates.append(contentsOf: progress(to: state, isMediaConnected: isMediaConnected))
        if let current = managed, current.isMuted != isMuted {
            managed?.isMuted = isMuted
            updates.append(.setMuted(current.call.id, isMuted))
        }
        return updates
    }

    /// The other side can replace an established call's session with a new
    /// one, which the slot accepts without asking. To the person, and so
    /// to the system, it is the same call under a new session id.
    private mutating func carryOverReplacedSession(to state: CallState) {
        guard let current = managed, !current.isAwaitingSlot,
            case .incoming(let next) = state, next.isAccepting, next.sid != current.call.sid,
            current.call.conversation == .direct(next.from.bare),
            // Only an established call is replaced; one still ringing or
            // being placed is not.
            current.call.direction == .incoming ? current.isAnswerKnown : current.progress >= .connecting
        else { return }
        managed?.call = SystemCall(
            id: current.call.id, sid: next.sid, direction: current.call.direction, conversation: current.call.conversation,
            isVideo: current.call.isVideo)
        // It was answered, or placed by us, long ago.
        managed?.isAnswerKnown = true
    }

    /// Only a call seen from its start is reported: ringing, being placed
    /// or being joined.
    private func newCall(for state: CallState, sid: String, isDoNotDisturb: Bool) -> SystemCall? {
        switch state {
        case .incoming(let call) where !call.isAccepting && !isDoNotDisturb:
            SystemCall(id: newID(), sid: sid, direction: .incoming, conversation: .direct(call.from.bare), isVideo: call.media.video)
        case .outgoing(let call):
            SystemCall(id: newID(), sid: sid, direction: .outgoing, conversation: .direct(call.to), isVideo: call.media.video)
        case .joiningGroup(let call):
            SystemCall(id: newID(), sid: sid, direction: .outgoing, conversation: .room(call.room), isVideo: call.media.video)
        case .idle, .incoming, .active, .inGroup, .ended:
            nil
        }
    }

    private mutating func progress(to state: CallState, isMediaConnected: Bool) -> [SystemCallUpdate] {
        guard var current = managed else { return [] }
        defer { managed = current }
        let id = current.call.id
        if current.call.direction == .incoming {
            // Told once the call is really answered: an accept still on its
            // way can fail and leave the call ringing. The system counts an
            // answered call as connected by itself.
            guard !state.isIncoming, !current.isAnswerKnown else { return [] }
            current.isAnswerKnown = true
            return [.answer(id)]
        }
        let reached: Progress = switch state {
        case .outgoing(let call): call.responder == nil ? .started : .connecting
        case .joiningGroup: .started
        case .active, .inGroup: isMediaConnected ? .connected : .connecting
        case .idle, .incoming, .ended: .started
        }
        var updates: [SystemCallUpdate] = []
        if current.progress < .connecting, reached >= .connecting { updates.append(.connecting(id)) }
        if current.progress < .connected, reached >= .connected { updates.append(.connected(id)) }
        current.progress = max(current.progress, reached)
        return updates
    }

    private static func end(of state: CallState, for call: SystemCall) -> SystemCallEnd {
        guard case .ended(call.sid, let reason) = state else { return .remoteEnded }
        switch reason {
        case .missed, .timeout: return .unanswered
        case .expired: return .answeredElsewhere
        case .error: return .failed
        case .rejected, .retracted: return .remoteEnded
        case .finished(let condition):
            switch condition {
            case .connectivityError, .failedTransport, .mediaError, .failedApplication, .generalError,
                .incompatibleParameters, .securityError, .unsupportedApplications, .unsupportedTransports:
                return .failed
            case .alternativeSession: return .answeredElsewhere
            case .timeout, .expired: return .unanswered
            case nil, .success, .busy, .cancel, .decline, .gone: return .remoteEnded
            }
        }
    }
}
