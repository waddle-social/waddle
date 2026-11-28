/// A microphone or camera start, including a fresh look for the device on retry.
@MainActor
public enum CallCaptureAttempt {
    public enum Outcome: Equatable, Sendable {
        case started
        case failed(CallMediaIssue.Reason)
        /// The call or the choice to turn on the device changed while we waited.
        case superseded
    }

    /// Availability is unknown when the platform does not offer a device list
    /// or the list could not be read. In either case, let capture try.
    public static func run(
        isCurrent: () -> Bool, requestAccess: () async -> Bool, deviceAvailability: () async -> Bool?,
        start: () async -> CallMediaIssue.Reason?
    ) async -> Outcome {
        guard isCurrent() else { return .superseded }
        let allowed = await requestAccess()
        guard isCurrent() else { return .superseded }
        guard allowed else { return .failed(.denied) }

        let available = await deviceAvailability()
        guard isCurrent() else { return .superseded }
        guard available != false else { return .failed(.missing) }

        let failure = await start()
        guard isCurrent() else { return .superseded }
        if let failure { return .failed(failure) }
        return .started
    }
}
