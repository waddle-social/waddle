import WaddleKit

extension FFIInbound {
    /// Call routing requires a valid sender; malformed addresses never
    /// cross the bridge into the single-slot call store.
    static func callEvent(_ event: WaddleCallEvent) -> CallEvent? {
        guard let from = jid(event.from) else {
            BridgeLog.debug("dropped call event: invalid sender JID")
            return nil
        }
        return CallEvent(from: from, sid: event.sid, kind: callKind(event.kind))
    }

    private static func callKind(_ kind: WaddleCallEventKind) -> CallEvent.Kind {
        switch kind {
        case .propose(let media): return .propose(callMedia(media))
        case .ringing: return .ringing
        case .proceed: return .proceed
        case .reject(let reason, let tieBreak): return .reject(reason: reason.map(jingleReason), tieBreak: tieBreak)
        case .retract(let reason, let tieBreak): return .retract(reason: reason.map(jingleReason), tieBreak: tieBreak)
        case .finish(let reason, let migratedTo): return .finish(reason: reason.map(jingleReason), migratedTo: migratedTo)
        case .sessionInitiate(let join, let media): return .sessionInitiate(join: callJoin(join), media: callMedia(media))
        case .sessionAccept(let join, let media): return .sessionAccept(join: callJoin(join), media: callMedia(media))
        case .sessionTerminate(let reason): return .sessionTerminate(reason: reason.map(jingleReason))
        }
    }

    private static func callMedia(_ media: WaddleCallMedia) -> CallMedia {
        CallMedia(audio: media.audio, video: media.video)
    }

    private static func callJoin(_ join: WaddleLiveKitJoin) -> CallJoin {
        CallJoin(url: join.url, room: join.room, identity: join.identity, token: join.token)
    }

    static func jingleReason(_ reason: WaddleJingleReason) -> JingleReason {
        switch reason {
        case .alternativeSession: return .alternativeSession
        case .busy: return .busy
        case .cancel: return .cancel
        case .connectivityError: return .connectivityError
        case .decline: return .decline
        case .expired: return .expired
        case .failedApplication: return .failedApplication
        case .failedTransport: return .failedTransport
        case .generalError: return .generalError
        case .gone: return .gone
        case .incompatibleParameters: return .incompatibleParameters
        case .mediaError: return .mediaError
        case .securityError: return .securityError
        case .success: return .success
        case .timeout: return .timeout
        case .unsupportedApplications: return .unsupportedApplications
        case .unsupportedTransports: return .unsupportedTransports
        }
    }

    static func callTerminateOutcome(_ outcome: WaddleCallSessionTerminateOutcome) -> CallTerminateOutcome {
        switch outcome {
        case .ok: return .ok
        case .orphaned: return .orphaned
        case .error: return .error
        }
    }
}
