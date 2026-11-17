import Foundation

/// A call as the operating system's call screen knows it.
public struct SystemCall: Hashable, Sendable {
    public enum Direction: Hashable, Sendable { case incoming, outgoing }

    /// The system's name for the call; ours is `sid`.
    public let id: UUID
    public let sid: String
    public let direction: Direction
    /// The person, or the room whose call it is.
    public let peer: BareJID
    public let isRoom: Bool
    public let isVideo: Bool

    public init(id: UUID, sid: String, direction: Direction, peer: BareJID, isRoom: Bool, isVideo: Bool) {
        self.id = id
        self.sid = sid
        self.direction = direction
        self.peer = peer
        self.isRoom = isRoom
        self.isVideo = isVideo
    }
}

/// Why a call left the system's call screen without the person ending it there.
public enum SystemCallEnd: Hashable, Sendable {
    case remoteEnded, unanswered, answeredElsewhere, failed
}

/// What to tell the system about the call slot.
public enum SystemCallUpdate: Hashable, Sendable {
    case reportIncoming(SystemCall)
    case startOutgoing(SystemCall)
    /// The call was answered in the app, not on the system's screen.
    case answer(UUID)
    /// The other side picked up, or the room's mixer let us in.
    case connecting(UUID)
    case connected(UUID)
    case end(UUID, SystemCallEnd)
}

/// Keeps the system's call screen in step with the call slot, telling it
/// each thing once.
///
/// One call at a time, as the slot holds. A call is *managed* from the
/// moment it is reported until it ends or the system refuses it; whoever
/// owns audio hands it to the system only for a managed call.
///
/// Not every call is reported. One that arrives under Do Not Disturb stays
/// off the system's screen for good, as does one first seen already
/// answered, and one the system refused: those are left to the app.
public struct SystemCallTracker {
    private enum Progress: Comparable { case started, connecting, connected }

    private struct Managed {
        let call: SystemCall
        var isAnswerKnown = false
        var progress = Progress.started
    }

    private let newID: () -> UUID
    private var managed: Managed?
    /// The slot's call that the system is not told about.
    private var unmanagedSid: String?

    public init(newID: @escaping () -> UUID = { UUID() }) {
        self.newID = newID
    }

    /// The system knows about the slot's call.
    public var isManaging: Bool { managed != nil }
    public var managedCall: SystemCall? { managed?.call }

    /// Our session id for the system's call, while it lasts.
    public func sid(for id: UUID) -> String? {
        managed?.call.id == id ? managed?.call.sid : nil
    }

    /// The person answered on the system's screen, so it need not be told.
    public mutating func systemAnswered(_ id: UUID) {
        guard managed?.call.id == id else { return }
        managed?.isAnswerKnown = true
    }

    /// The person ended the call on the system's screen, so it need not be told.
    public mutating func systemEnded(_ id: UUID) {
        guard let call = managed?.call, call.id == id else { return }
        managed = nil
        unmanagedSid = call.sid
    }

    /// The system would not take the call: it is the app's alone now.
    public mutating func systemRefused(_ id: UUID) {
        systemEnded(id)
    }

    public mutating func update(state: CallState, isMediaConnected: Bool, isDoNotDisturb: Bool = false) -> [SystemCallUpdate] {
        var updates: [SystemCallUpdate] = []
        let live = state.isLive ? state.sid : nil
        if let current = managed, current.call.sid != live {
            updates.append(.end(current.call.id, Self.end(of: state, for: current.call)))
            managed = nil
        }
        if unmanagedSid != live { unmanagedSid = nil }
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
        return updates
    }

    /// Only a call seen from its start is reported: ringing, being placed
    /// or being joined.
    private func newCall(for state: CallState, sid: String, isDoNotDisturb: Bool) -> SystemCall? {
        switch state {
        case .incoming(let call) where !call.isAccepting && !isDoNotDisturb:
            SystemCall(id: newID(), sid: sid, direction: .incoming, peer: call.from.bare, isRoom: false, isVideo: call.media.video)
        case .outgoing(let call):
            SystemCall(id: newID(), sid: sid, direction: .outgoing, peer: call.to, isRoom: false, isVideo: call.media.video)
        case .joiningGroup(let call):
            SystemCall(id: newID(), sid: sid, direction: .outgoing, peer: call.room, isRoom: true, isVideo: call.media.video)
        case .idle, .incoming, .active, .inGroup, .ended:
            nil
        }
    }

    private mutating func progress(to state: CallState, isMediaConnected: Bool) -> [SystemCallUpdate] {
        guard var current = managed else { return [] }
        defer { managed = current }
        let id = current.call.id
        if current.call.direction == .incoming {
            // The system counts an answered call as connected by itself.
            let isAnswered = if case .incoming(let call) = state { call.isAccepting } else { true }
            guard isAnswered, !current.isAnswerKnown else { return [] }
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
