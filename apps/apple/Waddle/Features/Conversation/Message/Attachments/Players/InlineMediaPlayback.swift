import AVFoundation
import Observation
import WaddleKit

/// Owns one inline player and tracks its playback for the attachment controls.
@MainActor
@Observable
final class InlineMediaPlayback {
    private struct Owner: Equatable {
        let id: UUID
        weak var playback: InlineMediaPlayback?

        static func == (lhs: Self, rhs: Self) -> Bool { lhs.id == rhs.id }
    }

    // A thread, a timeline and a second window all share the same output.
    private static var selection = InlinePlaybackSelection<Owner>()
    @ObservationIgnored private lazy var owner = Owner(id: UUID(), playback: self)
    @ObservationIgnored private var endObserver: (any NSObjectProtocol)?
    @ObservationIgnored private var transportObserver: NSKeyValueObservation?
    @ObservationIgnored private var didReachEnd = false

    private(set) var player: AVPlayer?
    private(set) var isPlaying = false
    private(set) var isMuted = false
    private(set) var didFail = false
    private(set) var elapsed: Double = 0
    private(set) var duration: Double?

    func togglePlayback(url: URL?, currentCall: @escaping @MainActor () -> CallState) {
        if let player, player.timeControlStatus != .paused, !didReachEnd {
            player.pause()
            isPlaying = false
            return
        }
        guard let url else {
            didFail = true
            return
        }
        didFail = false
        let player = player ?? prepare(url: url, currentCall: currentCall)
        prepareSound(for: player, call: currentCall())
        if didReachEnd {
            didReachEnd = false
            elapsed = 0
            player.seek(to: .zero, toleranceBefore: .zero, toleranceAfter: .zero)
        }
        isPlaying = true
        player.play()
    }

    func seek(to seconds: Double) {
        guard let player, let duration else { return }
        elapsed = min(duration, max(0, seconds))
        // A paused seek to the slider's end may never post a playback-end notification.
        didReachEnd = elapsed == duration && player.timeControlStatus == .paused
        player.seek(to: CMTime(seconds: elapsed, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
    }

    func setMuted(_ muted: Bool) {
        guard let player else { return }
        player.isMuted = muted
        isMuted = muted
    }

    func updateCall(_ call: CallState, wasLive: Bool) {
        guard let player else { return }
        let sound = InlinePlaybackSound(call: call)
        if sound.pausesPlayback(wasCallLive: wasLive, isPlaying: player.timeControlStatus != .paused) {
            player.pause()
            isPlaying = false
        }
        if sound.startsMuted { setMuted(true) }
    }

    func stop() {
        Self.selection.stop(owner)
        detachPlayer()
    }

    func observe(_ observed: AVPlayer) async {
        while !Task.isCancelled, player === observed, let item = observed.currentItem {
            if item.status == .failed || observed.status == .failed {
                stop()
                didFail = true
                return
            }
            let total = item.duration.seconds
            if total.isFinite, total > 0, duration != total { duration = total }
            let position = observed.currentTime().seconds
            if position.isFinite, elapsed != position { elapsed = position }
            let playing = observed.timeControlStatus != .paused
            if isPlaying != playing { isPlaying = playing }
            if isMuted != observed.isMuted { isMuted = observed.isMuted }
            try? await Task.sleep(for: .milliseconds(250))
        }
    }

    private func prepareSound(for player: AVPlayer, call: CallState) {
        let sound = InlinePlaybackSound(call: call)
        #if os(iOS)
        if sound.configuresPlaybackCategory {
            let session = AVAudioSession.sharedInstance()
            if session.category != .playback { try? session.setCategory(.playback) }
        }
        #endif
        if sound.startsMuted { player.isMuted = true }
        isMuted = player.isMuted
    }

    private func prepare(url: URL, currentCall: @escaping @MainActor () -> CallState) -> AVPlayer {
        Self.selection.start(owner)?.playback?.detachPlayer()
        // Even creating an item can fetch metadata, so this path belongs to Play.
        let player = AVPlayer(url: url)
        #if os(iOS)
        player.audiovisualBackgroundPlaybackPolicy = .pauses
        #endif
        if elapsed > 0 {
            player.seek(to: CMTime(seconds: elapsed, preferredTimescale: 600))
        }
        self.player = player
        // AVKit's transport can also resume playback directly.
        transportObserver = player.observe(\.timeControlStatus, options: [.old, .new]) { [weak self] player, change in
            guard change.oldValue == .paused, let status = change.newValue, status != .paused else { return }
            Task { @MainActor [weak self, weak player] in
                guard let self, let player, self.player === player, player.timeControlStatus != .paused else { return }
                self.didReachEnd = false
                self.prepareSound(for: player, call: currentCall())
            }
        }
        if let item = player.currentItem {
            endObserver = NotificationCenter.default.addObserver(
                forName: AVPlayerItem.didPlayToEndTimeNotification, object: item, queue: .main
            ) { [weak self, weak player] _ in
                MainActor.assumeIsolated {
                    guard let self, let player, self.player === player else { return }
                    self.didReachEnd = true
                    self.isPlaying = false
                }
            }
        }
        return player
    }

    private func detachPlayer() {
        guard let player else { return }
        player.pause()
        transportObserver?.invalidate()
        transportObserver = nil
        if let endObserver {
            NotificationCenter.default.removeObserver(endObserver)
            self.endObserver = nil
        }
        let position = player.currentTime().seconds
        if didReachEnd {
            elapsed = 0
        } else if position.isFinite, position > 0 {
            elapsed = position
        }
        didReachEnd = false
        // A native full-screen controller can outlive its row. Remove its
        // item immediately so its controls cannot restart a displaced player.
        player.replaceCurrentItem(with: nil)
        self.player = nil
        isPlaying = false
    }
}
