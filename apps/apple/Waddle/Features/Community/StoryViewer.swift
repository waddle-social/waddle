import AVFoundation
import SwiftUI
import WaddleKit

extension View {
    /// Presents the stories, starting at `story`: over the whole screen on
    /// iPhone and iPad, as a sheet on a Mac.
    func storyViewer(_ story: Binding<Story?>) -> some View {
        #if os(iOS)
        fullScreenCover(item: story) { StoryViewer(start: $0).carryingCallDock() }
        #else
        sheet(item: story) { StoryViewer(start: $0).frame(width: 420, height: 720).carryingCallDock() }
        #endif
    }
}

/// One story at a time, on the dark media ground (Night's deep sea, whatever
/// the appearance). The sides of the story, or the arrow keys, go back and
/// on through the stories that were up when it opened; the middle, or
/// Space, pauses a video, and M mutes it. A video moves on when it ends.
/// Showing a story marks it seen.
struct StoryViewer: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var playthrough: StoryPlaythrough
    @State private var errorMessage: String?
    private let now: Date
    @FocusState private var isFocused: Bool
    @ScaledMetric(relativeTo: .body) private var glyph: CGFloat = 15

    init(start: Story) {
        _playthrough = State(initialValue: StoryPlaythrough(start: start))
        now = Date()
        loadsStories = true
        picture = nil
    }

    /// For snapshots: fixed stories, clock and picture (standing in for a
    /// video too), and no session traffic.
    init(playthrough: StoryPlaythrough, now: Date, picture: Image) {
        _playthrough = State(initialValue: playthrough)
        self.now = now
        loadsStories = false
        self.picture = picture
    }

    private let loadsStories: Bool
    private let picture: Image?

    var body: some View {
        let story = playthrough.current
        ZStack {
            Theme.Colors.seaDeep.ignoresSafeArea()
            StoryMedia(
                story: story,
                picture: picture,
                isPaused: playthrough.isPaused,
                isMuted: playthrough.isMuted,
                onProgress: { if playthrough.progress != $0 { playthrough.progress = $0 } },
                onEnd: { finish(playthrough.videoEnded(story.id)) }
            )
            .id(story.id)
            pager(story)
            VStack(spacing: 0) {
                header(story)
                Spacer(minLength: 0)
                footer(story)
            }
        }
        .environment(\.colorScheme, .dark)
        .environment(\.huddleGround, Theme.Colors.seaDeep)
        .focusable()
        .focusEffectDisabled()
        .focused($isFocused)
        .onKeyPress(.leftArrow) { step(-1); return .handled }
        .onKeyPress(.rightArrow) { step(1); return .handled }
        .onKeyPress(.space) {
            guard plays(story) else { return .ignored }
            playthrough.isPaused.toggle()
            return .handled
        }
        .onKeyPress("m") {
            guard plays(story) else { return .ignored }
            playthrough.isMuted.toggle()
            return .handled
        }
        .onKeyPress(.escape) { dismiss(); return .handled }
        .onAppear {
            isFocused = true
            if loadsStories {
                playthrough = StoryPlaythrough(start: story, among: session.community.activeStories(at: now))
            }
        }
        .task(id: story.id) {
            guard loadsStories else { return }
            await session.markStoryRead(story.id)
        }
    }

    /// A video the player can decode: everything but WebM.
    private func plays(_ story: Story) -> Bool {
        story.isVideo && !story.isWebM
    }

    // MARK: Parts

    /// A quarter at each side goes back or on, as in every stories viewer;
    /// the middle pauses a video, or goes on from a picture.
    private func pager(_ story: Story) -> some View {
        GeometryReader { proxy in
            let side = proxy.size.width / 4
            HStack(spacing: 0) {
                zone { step(-1) }
                    .frame(width: side)
                    .accessibilityLabel(Text("Previous story"))
                    .disabled(playthrough.index == 0)
                zone {
                    if plays(story) {
                        playthrough.isPaused.toggle()
                    } else {
                        step(1)
                    }
                }
                // The header's buttons say the same to VoiceOver.
                .accessibilityHidden(true)
                zone { step(1) }
                    .frame(width: side)
                    .accessibilityLabel(Text(isLast ? "Close stories" : "Next story"))
            }
        }
        .buttonStyle(.plain)
    }

    private func zone(_ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Color.clear.contentShape(Rectangle())
        }
    }

    private var isLast: Bool {
        playthrough.index == playthrough.stories.count - 1
    }

    private func header(_ story: Story) -> some View {
        VStack(spacing: Theme.Spacing.m) {
            progress(story)
            HStack(spacing: Theme.Spacing.m) {
                if let author = story.author {
                    JIDAvatar(jid: author, size: Theme.Size.rowAvatar)
                }
                Text(CommunityCopy.name(of: story.author))
                    .huddleText(Theme.Typography.author)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                if let age = CommunityCopy.age(of: story.postedAt, at: now) {
                    Text(age)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
                Spacer(minLength: 0)
                HStack(spacing: 0) {
                    if plays(story) {
                        control(
                            playthrough.isPaused ? "play.fill" : "pause.fill",
                            label: playthrough.isPaused ? "Play" : "Pause"
                        ) {
                            playthrough.isPaused.toggle()
                        }
                        control(
                            playthrough.isMuted ? "speaker.slash.fill" : "speaker.wave.2.fill",
                            label: playthrough.isMuted ? "Unmute" : "Mute"
                        ) {
                            playthrough.isMuted.toggle()
                        }
                    }
                    control("xmark", label: "Close") { dismiss() }
                }
            }
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.top, Theme.Spacing.m)
        .padding(.bottom, Theme.Spacing.xl)
        .background(LinearGradient(colors: [Theme.Colors.scrim, .clear], startPoint: .top, endPoint: .bottom))
    }

    /// One bar per story: those seen are full, a video fills as it plays.
    private func progress(_ story: Story) -> some View {
        HStack(spacing: Theme.Spacing.xs) {
            ForEach(playthrough.stories.indices, id: \.self) { position in
                StoryProgressBar(filled: filled(at: position, playing: plays(story)))
            }
        }
        .accessibilityElement()
        .accessibilityLabel(Text("Story \(playthrough.index + 1) of \(playthrough.stories.count)"))
        .accessibilityValue(plays(story) ? Text("\(Int(playthrough.progress * 100)) percent played") : Text(""))
    }

    private func filled(at position: Int, playing: Bool) -> Double {
        if position < playthrough.index { return 1 }
        if position > playthrough.index { return 0 }
        return playing ? playthrough.progress : 1
    }

    private func control(_ symbol: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: glyph, weight: .semibold))
                .foregroundStyle(Theme.Colors.fg)
                .frame(minWidth: Theme.Size.minHitTarget, minHeight: Theme.Size.minHitTarget)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(label))
        .help(label)
    }

    private func footer(_ story: Story) -> some View {
        let reactions = session.community.reactions(of: story.id)
        return VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            if let body = story.body {
                Text(body)
                    .huddleText(Theme.Typography.message)
                    .foregroundStyle(Theme.Colors.fg)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let errorMessage {
                Text(errorMessage)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            }
            StoryReactionBar(reactions: reactions) { react($0, to: story) }
        }
        .padding(Theme.Spacing.l)
        .padding(.top, Theme.Spacing.xl)
        .background(LinearGradient(colors: [.clear, Theme.Colors.scrim], startPoint: .top, endPoint: .bottom))
    }

    // MARK: Actions

    private func step(_ offset: Int) {
        errorMessage = nil
        finish(playthrough.step(offset))
    }

    private func finish(_ step: StoryPlaythrough.Step) {
        if step == .close { dismiss() }
    }

    private func react(_ emoji: String, to story: Story) {
        errorMessage = nil
        Task {
            do {
                try await session.toggleStoryReaction(emoji, on: story.id)
            } catch PortError.invalidRequest {
                errorMessage = "You can leave up to \(StoryReactions.limit) reactions on a story."
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't react. Try again.")
            }
        }
    }
}

/// A story's place in the row along the top of the viewer.
private struct StoryProgressBar: View {
    /// From 0 to 1.
    let filled: Double

    var body: some View {
        Capsule()
            .fill(Theme.Colors.fg.opacity(0.3))
            .overlay(alignment: .leading) {
                GeometryReader { proxy in
                    Capsule()
                        .fill(Theme.Colors.fg)
                        .frame(width: proxy.size.width * min(1, max(0, filled)))
                }
            }
            .frame(height: 3)
    }
}

/// A story's picture or video, fitted to the viewer.
private struct StoryMedia: View {
    let story: Story
    /// Shown in place of the fetched picture or video.
    let picture: Image?
    let isPaused: Bool
    let isMuted: Bool
    let onProgress: (Double) -> Void
    let onEnd: () -> Void

    var body: some View {
        if let picture {
            picture.resizable().scaledToFit()
        } else if story.isWebM {
            StoryMediaFailure(message: "This video can't be played here.", symbol: "film")
        } else if story.isVideo {
            StoryVideo(story: story, isPaused: isPaused, isMuted: isMuted, onProgress: onProgress, onEnd: onEnd)
        } else {
            AsyncImage(url: story.media) { phase in
                switch phase {
                case let .success(image):
                    image.resizable().scaledToFit()
                case .failure:
                    StoryMediaFailure(message: "This story didn't load.", symbol: "photo.badge.exclamationmark")
                default:
                    ProgressView().tint(Theme.Colors.fg)
                }
            }
            .accessibilityLabel(Text("Picture from \(CommunityCopy.name(of: story.author))"))
        }
    }
}

private struct StoryMediaFailure: View {
    let message: String
    let symbol: String

    var body: some View {
        Label(message, systemImage: symbol)
            .huddleText(Theme.Typography.body)
            .foregroundStyle(Theme.Colors.fgMuted)
    }
}

/// A story's video without the player's own controls, which the viewer's
/// tap zones would cover: the viewer pauses and mutes it, and hears how
/// far it has played and when it ends.
private struct StoryVideo: View {
    let story: Story
    let isPaused: Bool
    let isMuted: Bool
    let onProgress: (Double) -> Void
    let onEnd: () -> Void
    @State private var player: AVPlayer?
    @State private var didFail = false

    var body: some View {
        if didFail {
            StoryMediaFailure(message: "This video didn't load.", symbol: "film")
        } else {
            VideoSurface(player: player)
                .task { await play() }
                .onChange(of: isPaused) { _, paused in
                    if paused { player?.pause() } else { player?.play() }
                }
                .onChange(of: isMuted) { _, muted in player?.isMuted = muted }
                .onReceive(NotificationCenter.default.publisher(for: AVPlayerItem.didPlayToEndTimeNotification)) { note in
                    guard let item = player?.currentItem, note.object as? AVPlayerItem === item else { return }
                    onEnd()
                }
                .onDisappear { player?.pause() }
                .accessibilityElement()
                .accessibilityLabel(Text("Video from \(CommunityCopy.name(of: story.author))"))
        }
    }

    /// Starts the video, then reports how far it has played until it fails
    /// (a file that is gone, cannot be fetched or does not decode) or the
    /// story is left.
    private func play() async {
        let item = AVPlayerItem(url: story.media)
        let player = AVPlayer(playerItem: item)
        player.isMuted = isMuted
        self.player = player
        if !isPaused { player.play() }
        while !Task.isCancelled {
            if item.status == .failed {
                didFail = true
                return
            }
            let duration = item.duration.seconds
            if duration.isFinite, duration > 0 {
                onProgress(player.currentTime().seconds / duration)
            }
            try? await Task.sleep(for: .milliseconds(100))
        }
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
#endif

/// The reactions a story has, each a toggle, then the quick ones nobody
/// has used yet.
struct StoryReactionBar: View {
    let reactions: StoryReactions
    let onToggle: (String) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Theme.Spacing.m) {
                ForEach(reactions.counts, id: \.emoji) { reaction in
                    let isMine = reactions.mine.contains(reaction.emoji)
                    Button { onToggle(reaction.emoji) } label: {
                        Text("\(reaction.emoji) \(reaction.count)")
                    }
                    .buttonStyle(.huddleChip(isSelected: isMine))
                    .accessibilityLabel(Text(CommunityCopy.reactionLabel(reaction, isMine: isMine)))
                    .accessibilityAddTraits(isMine ? .isSelected : [])
                }
                ForEach(unused, id: \.self) { emoji in
                    Button { onToggle(emoji) } label: {
                        Text(emoji)
                    }
                    .buttonStyle(.huddleChip())
                    .accessibilityLabel(Text("React with \(emoji)"))
                }
            }
            // Room for the chips' outlines, which a scroll view clips.
            .padding(Theme.Spacing.xxs)
        }
    }

    private var unused: [String] {
        let used = Set(reactions.counts.map(\.emoji))
        return Theme.quickReactions.filter { !used.contains($0) }
    }
}
