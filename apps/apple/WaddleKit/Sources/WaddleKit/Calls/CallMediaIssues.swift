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

    /// Where the person changes what Waddle may use, which decides what they can be told to do.
    public enum Platform: Equatable, Sendable {
        /// System Settings; a change may take a relaunch to be seen.
        case mac
        /// Settings; the system closes the app when a privacy switch changes, so the call ends.
        case phone

        public static var current: Self {
            #if os(macOS)
            .mac
            #else
            .phone
            #endif
        }
    }

    /// What happened, what it meant for the call, and what to do about it.
    public func message(on platform: Platform = .current) -> String {
        switch (kind, reason) {
        case (.microphone, .denied): "Waddle is not allowed to use the microphone, so nobody hears you. \(Self.remedy(on: platform))"
        case (.microphone, .failed): "Your microphone could not be started, so nobody hears you."
        case (.camera, .denied): "Waddle is not allowed to use the camera, so nobody sees you. \(Self.remedy(on: platform))"
        case (.camera, .failed): "Your camera could not be started, so nobody sees you."
        case (.screen, .denied):
            "Waddle cannot share the screen until you allow it under Screen & System Audio Recording in System Settings and reopen Waddle."
        case (.screen, .failed): "That could not be shared. Try again."
        }
    }

    private static func remedy(on platform: Platform) -> String {
        switch platform {
        case .mac: "Allow it in System Settings, then check again; Waddle may need reopening first."
        case .phone: "Allow it in Settings. The system closes Waddle when you do, so you will need to join again."
        }
    }

    /// The same in a line, for a surface with no buttons: the dock and the sidebar's card.
    public var line: String {
        switch (kind, reason) {
        case (.microphone, .denied): "Waddle is not allowed to use the microphone. Change that in Settings."
        case (.microphone, .failed): "Your microphone could not be started."
        case (.camera, .denied): "Waddle is not allowed to use the camera. Change that in Settings."
        case (.camera, .failed): "Your camera could not be started."
        case (.screen, .denied): "Waddle is not allowed to record the screen. Change that in System Settings."
        case (.screen, .failed): "That could not be shared. Try again."
        }
    }

    /// What the device is called in a button's spoken name.
    public var deviceName: String {
        switch kind {
        case .microphone: "microphone"
        case .camera: "camera"
        case .screen: "screen"
        }
    }

    /// The button that tries again: for a device, checking whether it may
    /// be used now and turning it on if so; for the screen, picking again.
    public var retryTitle: String {
        kind == .screen ? "Share again" : "Check again"
    }

    /// Refused in the system's settings, which is where it is put right.
    public var opensSettings: Bool { reason == .denied }
}

public enum CallMediaIssues {
    /// What a call's media reports about our own devices.
    public struct Report: Equatable, Sendable {
        public var isMicrophoneBlocked = false
        public var isCameraBlocked = false
        public var isScreenBlocked = false
        public var didScreenShareFail = false
        /// How many times each device has been tried and refused or failed
        /// in this call, from whichever surface asked.
        public var refusals: [CallMediaIssue.Kind: Int] = [:]

        public init(
            isMicrophoneBlocked: Bool = false, isCameraBlocked: Bool = false, isScreenBlocked: Bool = false,
            didScreenShareFail: Bool = false, refusals: [CallMediaIssue.Kind: Int] = [:]
        ) {
            self.isMicrophoneBlocked = isMicrophoneBlocked
            self.isCameraBlocked = isCameraBlocked
            self.isScreenBlocked = isScreenBlocked
            self.didScreenShareFail = didScreenShareFail
            self.refusals = refusals
        }
    }

    /// The notices a person has put away, each with how often its device
    /// had been refused by then. One more refusal, from the stage, the dock
    /// or the sidebar's card alike, is news, and the notice is back.
    public struct Dismissals: Equatable, Sendable {
        private var refusalsWhenDismissed: [CallMediaIssue.Kind: Int] = [:]

        public init() { /* Nothing put away. */ }

        public mutating func dismiss(_ kind: CallMediaIssue.Kind, in report: Report) {
            refusalsWhenDismissed[kind] = report.refusals[kind] ?? 0
        }

        public func hides(_ kind: CallMediaIssue.Kind, in report: Report) -> Bool {
            refusalsWhenDismissed[kind].map { $0 >= report.refusals[kind] ?? 0 } ?? false
        }
    }

    /// The notices to show, the microphone's first: without it nobody
    /// hears us. One the person put away for this call is left out.
    public static func shown(for report: Report, dismissed: Dismissals = Dismissals()) -> [CallMediaIssue] {
        var issues: [CallMediaIssue] = []
        if report.isMicrophoneBlocked { issues.append(CallMediaIssue(kind: .microphone, reason: .denied)) }
        if report.isCameraBlocked { issues.append(CallMediaIssue(kind: .camera, reason: .denied)) }
        if report.isScreenBlocked {
            issues.append(CallMediaIssue(kind: .screen, reason: .denied))
        } else if report.didScreenShareFail {
            issues.append(CallMediaIssue(kind: .screen, reason: .failed))
        }
        return issues.filter { !dismissed.hides($0.kind, in: report) }
    }

    /// One line for a surface with no room for a list, as the dock and the
    /// sidebar's card: the first notice's words.
    public static func line(for report: Report) -> String? {
        shown(for: report).first?.line
    }
}
