#if os(iOS)
import AVFoundation
import LiveKit

/// Who owns the audio session on an iPhone or iPad.
///
/// For a call the system's call screen manages, CallKit activates and
/// deactivates the session, and the media engine may only run in between:
/// started earlier it would have no session, and left running after it
/// would hold one the system has taken back. For any other call the
/// media engine configures the session itself, as it does by default.
@MainActor
enum SystemCallAudio {
    private static var isManaged = false
    private static var isActivated = false
    private static var microphoneWait: Task<Void, Never>?

    /// Hands the session to the system, or takes it back for the app.
    static func setManaged(_ managed: Bool) {
        guard isManaged != managed else { return }
        isManaged = managed
        AudioManager.shared.audioSession.isAutomaticConfigurationEnabled = !managed
        if managed {
            if !isActivated { stopEngine() }
        } else {
            microphoneWait?.cancel()
            try? AudioManager.shared.setEngineAvailability(.default)
        }
    }

    /// CallKit activated the session for the call.
    static func activated(_ session: AVAudioSession) {
        isActivated = true
        guard isManaged else { return }
        try? session.setCategory(.playAndRecord, mode: .voiceChat, options: [.mixWithOthers])
        startEngine()
    }

    /// CallKit took the session back.
    static func deactivated() {
        isActivated = false
        if isManaged { stopEngine() }
    }

    /// The engine refuses to start with input while the microphone prompt
    /// is still up, so the call is heard at once and the microphone joins
    /// when the person has answered the prompt.
    private static func startEngine() {
        microphoneWait?.cancel()
        if (try? AudioManager.shared.setEngineAvailability(.default)) != nil { return }
        try? AudioManager.shared.setEngineAvailability(.init(isInputAvailable: false, isOutputAvailable: true))
        microphoneWait = Task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(500))
                guard !Task.isCancelled, isManaged, isActivated else { return }
                if (try? AudioManager.shared.setEngineAvailability(.default)) != nil { return }
            }
        }
    }

    private static func stopEngine() {
        microphoneWait?.cancel()
        try? AudioManager.shared.setEngineAvailability(.none)
    }
}
#endif
