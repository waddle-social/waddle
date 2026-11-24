/// Something of ours a call could not use (web `call-media-issues`): the
/// microphone, the camera or the screen. Shown as a notice that can be
/// checked again or put away, never as an error: the call goes on.
public struct CallMediaIssue: Equatable, Identifiable, Sendable {
    public enum Kind: String, CaseIterable, Hashable, Sendable {
        case microphone, camera, screen
    }

    public enum Reason: Equatable, Sendable {
        /// The person has not allowed Waddle the device.
        case denied
        /// It was allowed and still did not start.
        case failed
    }

    public let kind: Kind
    public let reason: Reason

    public var id: Kind { kind }

    public init(kind: Kind, reason: Reason) {
        self.kind = kind
        self.reason = reason
    }

    /// What happened, what it meant for the call, and what to do about it.
    public var message: String {
        switch (kind, reason) {
        case (.microphone, .denied):
            "Waddle is not allowed to use the microphone, so you joined as a listener. Allow it in Settings, then check again."
        case (.microphone, .failed):
            "Your microphone could not be started, so you joined as a listener."
        case (.camera, .denied):
            "Waddle is not allowed to use the camera, so you joined without video. Allow it in Settings, then check again."
        case (.camera, .failed):
            "Your camera could not be started, so you joined without video."
        case (.screen, .denied):
            "Waddle cannot share the screen until you allow it under Screen & System Audio Recording in System Settings and reopen Waddle."
        case (.screen, .failed):
            "That could not be shared. Try again."
        }
    }

    /// The button that tries again: for a device, checking whether it may
    /// be used now and turning it on if so; for the screen, picking again.
    public var retryTitle: String {
        kind == .screen ? "Share again" : "Check again"
    }
}

public enum CallMediaIssues {
    /// What a call's media reports about our own devices.
    public struct Report: Equatable, Sendable {
        public var isMicrophoneBlocked = false
        public var isCameraBlocked = false
        public var isScreenBlocked = false
        public var didScreenShareFail = false

        public init(
            isMicrophoneBlocked: Bool = false, isCameraBlocked: Bool = false, isScreenBlocked: Bool = false,
            didScreenShareFail: Bool = false
        ) {
            self.isMicrophoneBlocked = isMicrophoneBlocked
            self.isCameraBlocked = isCameraBlocked
            self.isScreenBlocked = isScreenBlocked
            self.didScreenShareFail = didScreenShareFail
        }
    }

    /// The notices to show, the microphone's first: without it nobody
    /// hears us. One the person put away for this call is left out.
    public static func shown(for report: Report, dismissed: Set<CallMediaIssue.Kind> = []) -> [CallMediaIssue] {
        var issues: [CallMediaIssue] = []
        if report.isMicrophoneBlocked { issues.append(CallMediaIssue(kind: .microphone, reason: .denied)) }
        if report.isCameraBlocked { issues.append(CallMediaIssue(kind: .camera, reason: .denied)) }
        if report.isScreenBlocked {
            issues.append(CallMediaIssue(kind: .screen, reason: .denied))
        } else if report.didScreenShareFail {
            issues.append(CallMediaIssue(kind: .screen, reason: .failed))
        }
        return issues.filter { !dismissed.contains($0.kind) }
    }

    /// One line for a surface with no room for a list, as the dock and the
    /// sidebar's card: the first notice's words.
    public static func line(for report: Report) -> String? {
        shown(for: report).first?.message
    }
}
