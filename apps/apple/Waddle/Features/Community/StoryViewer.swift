import AVKit
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

/// One story at a time, on a dark ground: tap or arrow through the stories
/// that were up when it opened. Showing a story marks it seen.
struct StoryViewer: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var stories: [Story]
    @State private var index: Int
    @State private var errorMessage: String?
    private let now: Date
    @FocusState private var isFocused: Bool

    init(start: Story) {
        _stories = State(initialValue: [start])
        _index = State(initialValue: 0)
        now = Date()
        loadsStories = true
        picture = nil
    }

    /// For snapshots: fixed stories, clock and picture, and no session
    /// traffic.
    init(start: Story, stories: [Story], now: Date, picture: Image) {
        _stories = State(initialValue: stories)
        _index = State(initialValue: stories.firstIndex(of: start) ?? 0)
        self.now = now
        loadsStories = false
        self.picture = picture
    }

    private let loadsStories: Bool
    private let picture: Image?

    var body: some View {
        let story = stories[index]
        ZStack {
            Color.black.ignoresSafeArea()
            StoryMedia(story: story, picture: picture)
                .id(story.id)
            pager
            VStack(spacing: 0) {
                header(story)
                Spacer(minLength: 0)
                footer(story)
            }
        }
        .environment(\.colorScheme, .dark)
        .environment(\.huddleGround, .black)
        .focusable()
        .focusEffectDisabled()
        .focused($isFocused)
        .onKeyPress(.leftArrow) { step(-1); return .handled }
        .onKeyPress(.rightArrow) { step(1); return .handled }
        .onKeyPress(.escape) { dismiss(); return .handled }
        .onAppear {
            isFocused = true
            if loadsStories {
                let active = session.community.activeStories(at: now)
                if let start = active.firstIndex(where: { $0.id == story.id }) {
                    stories = active
                    index = start
                }
            }
        }
        .task(id: story.id) {
            guard loadsStories else { return }
            await session.markStoryRead(story.id)
        }
    }

    // MARK: Parts

    /// Left third goes back, the rest goes on, as in every stories viewer.
    private var pager: some View {
        HStack(spacing: 0) {
            Button { step(-1) } label: { Color.clear.contentShape(Rectangle()) }
                .frame(maxWidth: .infinity)
                .accessibilityLabel(Text("Previous story"))
                .disabled(index == 0)
            Button { step(1) } label: { Color.clear.contentShape(Rectangle()) }
                .frame(maxWidth: .infinity)
                .layoutPriority(2)
                .accessibilityLabel(Text(index == stories.count - 1 ? "Close stories" : "Next story"))
        }
        .buttonStyle(.plain)
    }

    private func header(_ story: Story) -> some View {
        VStack(spacing: Theme.Spacing.m) {
            HStack(spacing: Theme.Spacing.xs) {
                ForEach(stories.indices, id: \.self) { position in
                    Capsule()
                        .fill(Color.white.opacity(position <= index ? 0.95 : 0.3))
                        .frame(height: 3)
                }
            }
            .accessibilityElement()
            .accessibilityLabel(Text("Story \(index + 1) of \(stories.count)"))
            HStack(spacing: Theme.Spacing.m) {
                if let author = story.author {
                    JIDAvatar(jid: author, size: Theme.Size.rowAvatar)
                }
                Text(CommunityCopy.name(of: story.author))
                    .huddleText(Theme.Typography.author)
                    .foregroundStyle(.white)
                    .lineLimit(1)
                if let age = CommunityCopy.age(of: story.postedAt, at: now) {
                    Text(age)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(.white.opacity(0.7))
                }
                Spacer(minLength: 0)
                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(.white)
                        .huddleTouchTarget()
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text("Close"))
            }
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.top, Theme.Spacing.m)
        .padding(.bottom, Theme.Spacing.xl)
        .background(LinearGradient(colors: [.black.opacity(0.6), .clear], startPoint: .top, endPoint: .bottom))
    }

    private func footer(_ story: Story) -> some View {
        let reactions = session.community.reactions(of: story.id)
        return VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            if let body = story.body {
                Text(body)
                    .huddleText(Theme.Typography.message)
                    .foregroundStyle(.white)
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
        .background(LinearGradient(colors: [.clear, .black.opacity(0.7)], startPoint: .top, endPoint: .bottom))
    }

    // MARK: Actions

    private func step(_ offset: Int) {
        let next = index + offset
        guard next >= 0 else { return }
        guard next < stories.count else {
            dismiss()
            return
        }
        errorMessage = nil
        index = next
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

/// A story's picture or video, fitted to the viewer.
private struct StoryMedia: View {
    let story: Story
    /// Shown in place of the fetched picture.
    let picture: Image?
    @State private var player: AVPlayer?

    var body: some View {
        if let picture {
            picture.resizable().scaledToFit()
        } else if story.isWebM {
            Label("This video can't be played here.", systemImage: "film")
                .huddleText(Theme.Typography.body)
                .foregroundStyle(.white.opacity(0.7))
        } else if story.isVideo {
            VideoPlayer(player: player)
                .onAppear {
                    let player = AVPlayer(url: story.media)
                    self.player = player
                    player.play()
                }
                .onDisappear { player?.pause() }
                .accessibilityLabel(Text("Video from \(CommunityCopy.name(of: story.author))"))
        } else {
            AsyncImage(url: story.media) { phase in
                switch phase {
                case let .success(image):
                    image.resizable().scaledToFit()
                case .failure:
                    Label("This story didn't load.", systemImage: "photo.badge.exclamationmark")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(.white.opacity(0.7))
                default:
                    ProgressView().tint(.white)
                }
            }
            .accessibilityLabel(Text("Picture from \(CommunityCopy.name(of: story.author))"))
        }
    }
}

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
