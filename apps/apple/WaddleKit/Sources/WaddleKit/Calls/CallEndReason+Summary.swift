public extension CallEndReason {
    /// Why the call ended, in a few plain words for the person on it. Each
    /// XEP-0166 condition reads as what it means to them, as on Android.
    var summary: String {
        switch self {
        case .rejected: "Call declined"
        case .retracted: "Call cancelled"
        case .expired: "Answered on another device"
        case .timeout: "No answer"
        case .error: "Call could not connect"
        case .finished(let reason): Self.summary(of: reason)
        }
    }

    private static func summary(of reason: JingleReason?) -> String {
        switch reason {
        case nil, .success: "Call ended"
        case .decline: "Call declined"
        case .busy: "Busy right now"
        case .cancel: "Call cancelled"
        case .timeout, .expired: "No answer"
        case .alternativeSession: "Answered on another device"
        case .connectivityError, .failedTransport, .mediaError, .gone: "Connection lost"
        case .failedApplication, .generalError, .incompatibleParameters, .securityError,
            .unsupportedApplications, .unsupportedTransports:
            "Call could not connect"
        }
    }
}
