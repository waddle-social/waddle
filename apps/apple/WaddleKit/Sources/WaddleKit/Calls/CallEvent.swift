/// Tracks named by a call invitation or Jingle session.
public struct CallMedia: Hashable, Sendable {
    public let audio: Bool
    public let video: Bool
    public init(audio: Bool, video: Bool) {
        self.audio = audio
        self.video = video
    }
}

/// Server-issued SFU credentials from the Jingle transport.
public struct CallJoin: Hashable, Sendable {
    public let url: String
    public let room: String
    public let identity: String
    public let token: String
    public init(url: String, room: String, identity: String, token: String) {
        self.url = url
        self.room = room
        self.identity = identity
        self.token = token
    }
}

/// The seventeen XEP-0166 termination conditions.
public enum JingleReason: Hashable, Sendable, CaseIterable {
    case alternativeSession, busy, cancel, connectivityError, decline, expired
    case failedApplication, failedTransport, generalError, gone, incompatibleParameters
    case mediaError, securityError, success, timeout, unsupportedApplications, unsupportedTransports
}

/// A parsed live XEP-0353 invitation or XEP-0166 session event.
public struct CallEvent: Hashable, Sendable {
    public let from: JID
    public let sid: String
    public let kind: Kind
    public init(from: JID, sid: String, kind: Kind) {
        self.from = from
        self.sid = sid
        self.kind = kind
    }
    public enum Kind: Hashable, Sendable {
        case propose(CallMedia)
        case ringing, proceed
        case reject(reason: JingleReason?, tieBreak: Bool)
        case retract(reason: JingleReason?, tieBreak: Bool)
        case finish(reason: JingleReason?, migratedTo: String?)
        case sessionInitiate(join: CallJoin, media: CallMedia)
        case sessionAccept(join: CallJoin, media: CallMedia)
        case sessionTerminate(reason: JingleReason?)
    }
}

/// How the server answered a XEP-0166 session-terminate.
public enum CallTerminateOutcome: Hashable, Sendable {
    case ok
    /// The server no longer knows the session (crashed peer, recovered
    /// session); the `<finish/>` bookend still goes out.
    case orphaned
    case error
}
