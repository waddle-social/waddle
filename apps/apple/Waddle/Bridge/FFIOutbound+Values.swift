import Foundation
import WaddleKit

extension FFIOutbound {
    /// Exhaustive so a new FFI outcome is a compile error until mapped.
    static func sendOutcome(_ outcome: WaddleSendMessageOutcome) -> SendOutcome {
        switch outcome {
        case let .sent(stanzaId): return .sent(stanzaID: stanzaId)
        case .notConnected: return .notConnected
        case .transportError: return .transportError
        case .invalidRecipient, .invalidOptions, .stanzaError, .error: return .rejected
        }
    }

    static func portError(_ error: Error) -> PortError {
        if let portError = error as? PortError { return portError }
        guard let ffiError = error as? WaddleError else { return .failed }
        switch ffiError {
        case .NotConnected: return .notConnected
        case .Timeout: return .timeout
        case .Stanza: return .rejected
        case .InvalidJid, .InvalidArgument, .InvalidSessionId: return .invalidRequest
        case .MalformedResponse, .Transport, .UntrustedReply: return .failed
        }
    }

    static func notifyMode(_ mode: NotifyMode) -> WaddleNotifyMode {
        switch mode {
        case .always: return .always
        case .onMention: return .onMention
        case .never: return .never
        }
    }

    static func affiliation(_ affiliation: RoomAffiliation) -> WaddleMucAffiliation {
        switch affiliation {
        case .owner: return .owner
        case .admin: return .admin
        case .member: return .member
        case .none: return .none
        case .outcast: return .outcast
        }
    }

    static func chatState(_ state: ChatState) -> WaddleChatState {
        switch state {
        case .active: return .active
        case .composing: return .composing
        case .paused: return .paused
        case .inactive: return .inactive
        case .gone: return .gone
        }
    }

    static func pushEnvironment(_ environment: PushEnvironment) -> WaddlePushEnvironment {
        switch environment {
        case .production: return .production
        case .sandbox: return .sandbox
        }
    }
}

/// Runs an FFI call, rethrowing its failure as the `PortError` callers
/// branch on.
func mappingPortErrors<Value>(_ operation: () async throws -> Value) async throws -> Value {
    do {
        return try await operation()
    } catch {
        throw FFIOutbound.portError(error)
    }
}
