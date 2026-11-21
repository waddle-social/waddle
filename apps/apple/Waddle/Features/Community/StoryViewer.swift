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
/// Space, pauses a video, and M mutes it. A video moves on when it ends,
/// and is silent while a call rings or is on. Showing a story marks it
/// seen.
struct StoryViewer: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var playthrough: StoryPlaythrough
    @State private var video: StoryVideoPlayback?
    @State private var errorMessage: String?
    #if os(iOS)
    @State private var audio = StoryAudio(session: SharedStoryAudioSession())
    #endif
    private let now: Date
    private let loadsStories: Bool
    private let standIn: Image?

    init(start: Story) {
        _playthrough = State(initialValue: StoryPlaythrough(start: start))
        now = Date()
        loadsStories = true
        standIn = nil
    }

    /// For snapshots: fixed stories and clock, `standIn` drawn in place of
    /// the story's picture or video, `video` the state its player is in,
    /// and no session traffic.
    init(playthrough: StoryPlaythrough, now: Date, standIn: Image, video: StoryVideoPlayback? = nil) {
        _playthrough = State(initialValue: playthrough)
        _video = State(initialValue: video)
        self.now = now
        loadsStories = false
        self.standIn = standIn
    }

    var body: some View {
        let story = playthrough.current
        let sound = StorySound(during: session.calls.state)
        let playing = playable(story)
        ZStack {
            Theme.Colors.seaDeep.ignoresSafeArea()
            media(story)
                .id(story.id)
            pager(playing)
            VStack(spacing: 0) {
                header(story, video: playing, sound: sound)
                Spacer(minLength: 0)
                footer(story)
            }
        }
        .environment(\.colorScheme, .dark)
        .environment(\.huddleGround, Theme.Colors.seaDeep)
        .onAppear {
            if loadsStories {
                playthrough = StoryPlaythrough(start: story, among: session.community.activeStories(at: now))
            }
            updateAudio(sound)
        }
        .onChange(of: sound) { _, sound in
            // The session is the stories' again before the video is heard.
            updateAudio(sound)
            video?.setMuted(isMuted(sound))
        }
        .onChange(of: playthrough.isMuted) { _, _ in video?.setMuted(isMuted(sound)) }
        .onDisappear {
            video?.pause()
            #if os(iOS)
            audio.close()
            #endif
        }
        .task(id: story.id) {
            guard loadsStories else { return }
            await session.markStoryRead(story.id)
        }
        .task(id: story.id) { await play(story) }
    }

    /// The shown story's video while its player can play it. A video that
    /// failed is treated like a picture.
    private func playable(_ story: Story) -> StoryVideoPlayback? {
        guard let video, video.storyID == story.id, !video.didFail else { return nil }
        return video
    }

    private func isMuted(_ sound: StorySound) -> Bool {
        playthrough.isMuted || sound == .silencedByCall
    }

    private func updateAudio(_ sound: StorySound) {
        #if os(iOS)
        audio.update(sound)
        #endif
    }

    // MARK: Parts

    @ViewBuilder
    private func media(_ story: Story) -> some View {
        if let standIn {
            standIn.resizable().scaledToFit()
        } else {
            switch story.kind {
            case .picture:
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
            case .webM:
                StoryMediaFailure(
                    message: "This video is in a format Apple devices can't play.",
                    symbol: "film"
                )
            case .video:
                if let video, video.storyID == story.id {
                    StoryVideo(playback: video, author: story.author)
                } else {
                    ProgressView().tint(Theme.Colors.fg)
                }
            }
        }
    }

    /// A quarter at each side goes back or on, as in every stories viewer;
    /// the middle pauses a video, or goes on from a picture. The keys are
    /// shortcuts on these buttons, so they work wherever focus is.
    private func pager(_ video: StoryVideoPlayback?) -> some View {
        GeometryReader { proxy in
            let side = proxy.size.width / 4
            HStack(spacing: 0) {
                zone(.leftArrow) { move(by: -1) }
                    .frame(width: side)
                    .accessibilityLabel(Text("Previous story"))
                    .disabled(playthrough.index == 0)
                zone(.space) {
                    if let video {
                        video.togglePause()
                    } else {
                        move(by: 1)
                    }
                }
                // The header's buttons say the same to VoiceOver.
                .accessibilityHidden(true)
                zone(.rightArrow) { move(by: 1) }
                    .frame(width: side)
                    .accessibilityLabel(Text(playthrough.isLast ? "Close stories" : "Next story"))
            }
        }
        .buttonStyle(.plain)
    }

    private func zone(_ key: KeyEquivalent, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Color.clear.contentShape(Rectangle())
        }
        .keyboardShortcut(key, modifiers: [])
    }

    private func header(_ story: Story, video: StoryVideoPlayback?, sound: StorySound) -> some View {
        VStack(spacing: Theme.Spacing.m) {
            StoryProgressBars(playthrough: playthrough, video: video)
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
                    if let video {
                        HuddleIconButton(
                            video.isPaused ? "Play" : "Pause",
                            symbol: video.isPaused ? "play.fill" : "pause.fill"
                        ) {
                            video.togglePause()
                        }
                        muteButton(sound)
                    }
                    HuddleIconButton("Close", symbol: "xmark", action: close)
                        .keyboardShortcut(.cancelAction)
                }
                .foregroundStyle(Theme.Colors.fg)
            }
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.top, Theme.Spacing.m)
        .padding(.bottom, Theme.Spacing.xl)
        .background(LinearGradient(colors: [Theme.Colors.scrim, .clear], startPoint: .top, endPoint: .bottom))
    }

    private func muteButton(_ sound: StorySound) -> some View {
        let muted = isMuted(sound)
        return HuddleIconButton(
            muted ? "Unmute" : "Mute",
            symbol: muted ? "speaker.slash.fill" : "speaker.wave.2.fill"
        ) {
            playthrough.isMuted.toggle()
        }
        .keyboardShortcut("m", modifiers: [])
        .disabled(sound == .silencedByCall)
        .accessibilityHint(sound == .silencedByCall ? Text("Story sound is off during a call.") : Text(""))
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

    /// Starts the shown story's video; the task ends, and the player
    /// pauses, when the story is left.
    private func play(_ story: Story) async {
        guard loadsStories, story.kind == .video else { return }
        let playback = StoryVideoPlayback(
            storyID: story.id, url: story.media, isMuted: isMuted(StorySound(during: session.calls.state))
        )
        playback.onEnd = { closeIfFinished(playthrough.videoEnded(story.id)) }
        video = playback
        await playback.run()
    }

    private func move(by offset: Int) {
        errorMessage = nil
        closeIfFinished(playthrough.move(by: offset))
    }

    private func closeIfFinished(_ outcome: StoryPlaythrough.Outcome) {
        if outcome == .finished { close() }
    }

    /// Silent at once, not after the closing animation.
    private func close() {
        video?.pause()
        dismiss()
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

/// One bar per story along the top: those seen are full, and a video's
/// fills as it plays. The only part of the viewer that reads the video's
/// progress, so only it is drawn again as the video plays.
private struct StoryProgressBars: View {
    let playthrough: StoryPlaythrough
    let video: StoryVideoPlayback?

    var body: some View {
        let played = video?.progress
        HStack(spacing: Theme.Spacing.xs) {
            ForEach(playthrough.stories.indices, id: \.self) { position in
                bar(filled: playthrough.fill(at: position, played: played))
            }
        }
        .accessibilityElement()
        .accessibilityLabel(Text("Story \(playthrough.index + 1) of \(playthrough.stories.count)"))
        .accessibilityValue(played.map { Text("\(Int($0 * 100)) percent played") } ?? Text(""))
    }

    private func bar(filled: Double) -> some View {
        Capsule()
            .fill(Theme.Colors.fg.opacity(0.3))
            .overlay(alignment: .leading) {
                GeometryReader { proxy in
                    Capsule()
                        .fill(Theme.Colors.fg)
                        .frame(width: proxy.size.width * filled)
                }
            }
            .frame(height: 3)
    }
}
