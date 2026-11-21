import AVFoundation
import SwiftUI
import WaddleKit

/// A story video's player, and what the viewer shows of it. The player
/// says whether the video plays: the system pauses it too (the app going
/// to the background, an audio interruption), so the buttons act on what
/// it is doing, not on what was last asked of it.
@MainActor
@Observable
final class StoryVideoPlayback {
    let storyID: String
    let player: AVPlayer?
    private(set) var isPaused: Bool
    /// Until the first frame can be shown, and while the player waits for
    /// more of the file.
    private(set) var isLoading: Bool
    /// The file is gone, cannot be fetched or does not decode.
    private(set) var didFail = false
    /// How far it has played, from 0 to 1. Only the progress bar reads it,
    /// so only the bar is drawn again as it moves.
    private(set) var progress: Double
    /// Called when the video has played to its end.
    @ObservationIgnored var onEnd: () -> Void = {}

    init(storyID: String, url: URL, isMuted: Bool) {
        self.storyID = storyID
        let player = AVPlayer(url: url)
        player.isMuted = isMuted
        self.player = player
        isPaused = true
        isLoading = true
        progress = 0
    }

    /// For snapshots: no player, held as it is given.
    init(storyID: String, isPaused: Bool, progress: Double) {
        self.storyID = storyID
        player = nil
        self.isPaused = isPaused
        isLoading = false
        self.progress = progress
    }

    func togglePause() {
        guard let player else { return }
        if player.timeControlStatus == .paused {
            player.play()
        } else {
            player.pause()
        }
        refresh()
    }

    func pause() {
        player?.pause()
        refresh()
    }

    func setMuted(_ isMuted: Bool) {
        player?.isMuted = isMuted
    }

    /// Plays from the start and follows the player until the task is
    /// cancelled, then pauses it.
    func run() async {
        guard let player, let item = player.currentItem else { return }
        let ended = NotificationCenter.default.addObserver(
            forName: AVPlayerItem.didPlayToEndTimeNotification, object: item, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.onEnd() }
        }
        defer {
            NotificationCenter.default.removeObserver(ended)
            player.pause()
        }
        player.play()
        while !Task.isCancelled, !didFail {
            refresh()
            try? await Task.sleep(for: .milliseconds(100))
        }
    }

    /// Takes the player's state. Each value is set only when it changes,
    /// so views that read it are not drawn again for nothing.
    private func refresh() {
        guard let player, let item = player.currentItem else { return }
        let failed = item.status == .failed
        let paused = player.timeControlStatus == .paused
        let loading = !failed && (item.status == .unknown || player.timeControlStatus == .waitingToPlayAtSpecifiedRate)
        if didFail != failed { didFail = failed }
        if isPaused != paused { isPaused = paused }
        if isLoading != loading { isLoading = loading }
        let duration = item.duration.seconds
        if duration.isFinite, duration > 0 {
            let played = min(1, max(0, player.currentTime().seconds / duration))
            if progress != played { progress = played }
        }
    }
}

/// A story's video in a bare player layer (the system player's controls
/// would sit under the viewer's tap zones), with a spinner while it loads.
struct StoryVideo: View {
    let playback: StoryVideoPlayback
    let author: BareJID?

    var body: some View {
        if playback.didFail {
            StoryMediaFailure(message: "This video didn't load.", symbol: "film")
        } else {
            VideoSurface(player: playback.player)
                .overlay {
                    if playback.isLoading {
                        ProgressView().tint(Theme.Colors.fg)
                    }
                }
                .accessibilityElement()
                .accessibilityLabel(Text("Video from \(CommunityCopy.name(of: author))"))
        }
    }
}

/// What a story shows when its picture or video cannot be shown.
struct StoryMediaFailure: View {
    let message: String
    let symbol: String

    var body: some View {
        Label(message, systemImage: symbol)
            .huddleText(Theme.Typography.body)
            .foregroundStyle(Theme.Colors.fgMuted)
            .multilineTextAlignment(.center)
            .padding(Theme.Spacing.l)
    }
}

/// An `AVPlayerLayer`, fitted to the view.
#if os(macOS)
private struct VideoSurface: NSViewRepresentable {
    let player: AVPlayer?

    func makeNSView(context _: Context) -> PlayerView { PlayerView() }

    func updateNSView(_ view: PlayerView, context _: Context) {
        view.playerLayer.player = player
    }

    final class PlayerView: NSView {
        let playerLayer = AVPlayerLayer()

        init() {
            super.init(frame: .zero)
            playerLayer.videoGravity = .resizeAspect
            layer = playerLayer
            wantsLayer = true
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) { nil }
    }
}
#else
private struct VideoSurface: UIViewRepresentable {
    let player: AVPlayer?

    func makeUIView(context _: Context) -> PlayerView { PlayerView() }

    func updateUIView(_ view: PlayerView, context _: Context) {
        view.playerLayer.player = player
        view.playerLayer.videoGravity = .resizeAspect
    }

    final class PlayerView: UIView {
        override class var layerClass: AnyClass { AVPlayerLayer.self }
        var playerLayer: AVPlayerLayer { layer as! AVPlayerLayer }
    }
}

/// Outside a call, story video mixes with other apps' audio and follows
/// the silent switch (the `ambient` category), and the category it
/// replaced comes back when the viewer closes. During a call the session
/// belongs to the call (`SystemCallAudio`, the media engine): it is not
/// touched, and the video stays muted.
@MainActor
struct StoryAudioSession {
    private struct Setting {
        let category: AVAudioSession.Category
        let mode: AVAudioSession.Mode
        let options: AVAudioSession.CategoryOptions
    }

    private var replaced: Setting?

    mutating func begin(_ sound: StorySound) {
        guard sound == .mixed, replaced == nil else { return }
        let session = AVAudioSession.sharedInstance()
        guard session.category != .ambient else { return }
        replaced = Setting(category: session.category, mode: session.mode, options: session.categoryOptions)
        try? session.setCategory(.ambient)
    }

    mutating func end() {
        guard let replaced else { return }
        self.replaced = nil
        let session = AVAudioSession.sharedInstance()
        // A call that started meanwhile set its own category; it stays.
        guard session.category == .ambient else { return }
        try? session.setCategory(replaced.category, mode: replaced.mode, options: replaced.options)
    }
}
#endif
