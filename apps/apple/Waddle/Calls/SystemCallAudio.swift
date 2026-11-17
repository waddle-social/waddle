#if os(iOS)
import AVFoundation
import LiveKit
import WaddleKit

/// Runs the media engine for whoever owns audio on an iPhone or iPad
/// (`SystemCallAudioOwner`).
///
/// - The system: CallKit activates and deactivates the audio session, and
///   the engine may only run in between. Started earlier it would have no
///   session; left running after, it would hold one the system took back.
/// - The app: the engine configures the session itself, as it does by
///   default, and may run.
/// - Nobody: the engine may not run. A call that is over cannot start its
///   microphone again while its media is still being torn down.
@MainActor
final class SystemCallAudio {
    private var owner = SystemCallAudioOwner.nobody
    private var isActivated = false
    private var microphoneWait: Task<Void, Never>?

    init() {
        apply()
    }

    func setOwner(_ next: SystemCallAudioOwner) {
        guard owner != next else { return }
        // An activation belongs to the call it was for; the next call the
        // system manages waits for its own.
        if owner == .system { isActivated = false }
        owner = next
        apply()
    }

    /// CallKit activated the session for the call.
    func activated(_ session: AVAudioSession) {
        isActivated = true
        guard owner == .system else { return }
        // What the media engine sets for a call when it configures the
        // session itself: loudspeaker by default, Bluetooth and AirPlay
        // routes allowed.
        let call = AudioSessionConfiguration.playAndRecordSpeaker
        try? session.setCategory(call.category, mode: call.mode, options: call.categoryOptions)
        apply()
    }

    /// CallKit took the session back.
    func deactivated() {
        isActivated = false
        // A late deactivation for the last call lands on the one session
        // there is. If the app has a call running on it by now, the engine
        // is restarted so that it takes the session again.
        if owner == .app { stopEngine() }
        apply()
    }

    private func apply() {
        // Off only while the system owns the session; otherwise the engine
        // also releases the session when it stops.
        AudioManager.shared.audioSession.isAutomaticConfigurationEnabled = owner != .system
        switch owner {
        case .system: isActivated ? startEngine() : stopEngine()
        case .app: startEngine()
        case .nobody: stopEngine()
        }
    }

    /// The engine refuses to start with input while the microphone prompt
    /// is still up, so the call is heard at once and the microphone joins
    /// when the person has answered the prompt.
    private func startEngine() {
        microphoneWait?.cancel()
        if (try? AudioManager.shared.setEngineAvailability(.default)) != nil { return }
        try? AudioManager.shared.setEngineAvailability(.init(isInputAvailable: false, isOutputAvailable: true))
        microphoneWait = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(500))
                guard !Task.isCancelled, self != nil else { return }
                if (try? AudioManager.shared.setEngineAvailability(.default)) != nil { return }
            }
        }
    }

    private func stopEngine() {
        microphoneWait?.cancel()
        try? AudioManager.shared.setEngineAvailability(.none)
    }
}
#endif
