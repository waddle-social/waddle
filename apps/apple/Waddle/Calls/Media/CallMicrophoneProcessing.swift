import Foundation
import LiveKit
import Observation
import WaddleKit

@MainActor
@Observable
final class CallMicrophoneProcessing {
    @ObservationIgnored private let roomAccess: CallRoomAccess
    @ObservationIgnored private let defaults: UserDefaults
    private static let audioProcessingKey = "callAudioProcessing"
    private static let audioProcessingWrites = DispatchQueue(label: "waddle.call-audio-processing", qos: .userInitiated)

    private(set) var audioProcessing: CallAudioProcessing
    /// The processing the microphone's track last took, for tests: `nil` until one has.
    @ObservationIgnored private(set) var appliedAudioProcessing: CallAudioProcessing?
    /// The microphone publications that have the choice. A reconnect takes
    /// the publication away and publishes the track again, restarted with
    /// the options it was made with, so one that is not here has yet to be given it.
    @ObservationIgnored private var audioProcessingPublications: Set<String> = []

    init(roomAccess: CallRoomAccess, defaults: UserDefaults) {
        self.roomAccess = roomAccess
        self.defaults = defaults
        audioProcessing = CallAudioProcessing(stored: defaults.dictionary(forKey: Self.audioProcessingKey))
    }

    func setAudioProcessing(_ processing: CallAudioProcessing) async {
        guard processing != audioProcessing else { return }
        audioProcessing = processing
        defaults.set(processing.stored, forKey: Self.audioProcessingKey)
        await applyAudioProcessing()
    }

    var audioCaptureOptions: AudioCaptureOptions {
        AudioCaptureOptions(
            echoCancellation: audioProcessing.echoCancellation, autoGainControl: audioProcessing.autoGainControl,
            noiseSuppression: audioProcessing.noiseSuppression
        )
    }

    func roomDidGoAway() {
        appliedAudioProcessing = nil
        audioProcessingPublications = []
    }

    func roomDidReconnect() {
        // A reconnect restarts the microphone's track with the options it was made with.
        Task { await applyAudioProcessing() }
    }

    /// Gives the choice to a microphone publication that has not had it: one
    /// that came back after a reconnect, or appeared while a change found none.
    func tracksDidChange(in room: Room) {
        let current = Set(microphonePublications(in: room).map(\.sid.stringValue))
        guard !current.isEmpty, current != audioProcessingPublications || appliedAudioProcessing != audioProcessing else { return }
        Task { await applyAudioProcessing() }
    }

    /// Gives the processing chosen to the microphone's track as it is, on
    /// or muted: the SDK changes it in place, with no new track and no gap
    /// in what others hear. A track keeps the options it was made with for
    /// its own restarts, so this runs again after a reconnect.
    func applyAudioProcessing() async {
        guard let room = roomAccess.room else { return }
        let wanted = audioProcessing
        let options = AudioProcessingOptions(
            echoCancellation: wanted.echoCancellation, autoGainControl: wanted.autoGainControl,
            noiseSuppression: wanted.noiseSuppression
        )
        let published = microphonePublications(in: room)
        let tracks = published.compactMap { $0.track as? LocalAudioTrack }
        let publicationIDs = Set(published.map(\.sid.stringValue))
        guard !tracks.isEmpty else { return }
        // The SDK waits on WebRTC's signalling thread for each write, so
        // they leave the main thread; one queue keeps them in the order asked.
        let accepted = await withCheckedContinuation { (done: CheckedContinuation<Bool, Never>) in
            Self.audioProcessingWrites.async {
                let results = tracks.map { (try? $0.setAudioProcessingOptions(options)) != nil }
                done.resume(returning: !results.contains(false))
            }
        }
        // The latest choice may have moved on while this one was written; its own write follows.
        // And the call may have ended: what was written was written to its tracks.
        if accepted, wanted == audioProcessing, roomAccess.holds(room) {
            appliedAudioProcessing = wanted
            audioProcessingPublications = publicationIDs
        }
    }

    private func microphonePublications(in room: Room) -> [LocalTrackPublication] {
        room.localParticipant.localAudioTracks.filter { $0.source == .microphone }
    }
}
