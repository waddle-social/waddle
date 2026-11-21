import SwiftUI
import WaddleKit

/// The stories still up, newest first, behind a tile for adding your own.
struct StoryRail: View {
    @Environment(SessionCoordinator.self) private var session
    let stories: [Story]
    let now: Date
    let onAdd: () -> Void
    let onOpen: (Story) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(alignment: .top, spacing: Theme.Spacing.m) {
                Button(action: onAdd) {
                    StoryTile(title: "Your story") {
                        ZStack {
                            Circle().fill(Theme.Colors.actionSubtle)
                            Image(systemName: "plus")
                                .font(.system(size: 22, weight: .semibold))
                                .foregroundStyle(Theme.Colors.actionText)
                        }
                    }
                }
                .accessibilityLabel(Text("Add a story"))
                ForEach(stories) { story in
                    let isRead = session.community.isRead(story.id)
                    Button { onOpen(story) } label: {
                        StoryTile(title: CommunityCopy.name(of: story.author), isUnread: !isRead) {
                            avatar(of: story)
                        }
                    }
                    .accessibilityLabel(Text(CommunityCopy.storyLabel(story, isRead: isRead, at: now)))
                }
            }
            .buttonStyle(.plain)
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.xs)
        }
        .padding(.horizontal, -Theme.Spacing.l)
    }

    @ViewBuilder
    private func avatar(of story: Story) -> some View {
        if let author = story.author {
            JIDAvatar(jid: author, size: StoryTile<EmptyView>.portrait)
        } else {
            AvatarView(name: CommunityCopy.name(of: nil), size: StoryTile<EmptyView>.portrait)
        }
    }
}

/// A round portrait with a name under it. A story not seen yet wears the
/// action colour as a ring; a seen one a quiet border.
struct StoryTile<Portrait: View>: View {
    static var portrait: CGFloat { 56 }

    let title: String
    var isUnread = false
    @ViewBuilder let portrait: Portrait

    var body: some View {
        VStack(spacing: Theme.Spacing.xs) {
            portrait
                .frame(width: Self.portrait, height: Self.portrait)
                .padding(4)
                .overlay(
                    Circle().strokeBorder(
                        isUnread ? Theme.Colors.action : Theme.Colors.border,
                        lineWidth: isUnread ? 2.5 : 1
                    )
                )
            Text(title)
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(isUnread ? Theme.Colors.fg : Theme.Colors.fgMuted)
                .lineLimit(1)
        }
        .frame(width: Self.portrait + 20)
        .contentShape(Rectangle())
    }
}
