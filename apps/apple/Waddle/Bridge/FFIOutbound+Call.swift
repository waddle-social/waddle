import WaddleKit

extension FFIOutbound {
    static func jingleReason(_ reason: JingleReason) -> WaddleJingleReason {
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
}
