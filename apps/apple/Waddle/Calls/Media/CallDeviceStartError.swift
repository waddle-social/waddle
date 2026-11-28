import AVFoundation
import LiveKit
import WaddleKit

/// Translates typed system and SDK errors from a microphone or camera start.
enum CallDeviceStartError {
    static func reason(for error: Error) -> CallMediaIssue.Reason {
        if let liveKit = error as? LiveKitError {
            switch liveKit.type {
            case .deviceNotFound: return .missing
            case .deviceAccessDenied: return .denied
            default: break
            }
        }
        let native = error as NSError
        if native.domain == AVFoundationErrorDomain {
            switch AVError.Code(rawValue: native.code) {
            case .deviceAlreadyUsedByAnotherSession, .deviceInUseByAnotherApplication: return .inUse
            case .deviceNotConnected, .deviceWasDisconnected: return .missing
            case .applicationIsNotAuthorizedToUseDevice: return .denied
            default: break
            }
        }
        if let underlying = native.userInfo[NSUnderlyingErrorKey] as? NSError, underlying !== native {
            return reason(for: underlying)
        }
        return .failed
    }
}
