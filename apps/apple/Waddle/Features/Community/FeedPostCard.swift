import SwiftUI
import WaddleKit

/// One post in the Feed: who, when, and what they shared.
struct FeedPostCard: View {
    let post: FeedPost
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            HStack(spacing: Theme.Spacing.m) {
                avatar
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text(CommunityCopy.name(of: post.author))
                        .huddleText(Theme.Typography.author)
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(1)
                    Text(CommunityCopy.byline(for: post, at: now))
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .accessibilityElement(children: .combine)
            if let title = post.title, title != post.body, !post.body.hasPrefix(title) {
                Text(title)
                    .huddleText(Theme.Typography.subtitle)
                    .foregroundStyle(Theme.Colors.fg)
            }
            Text(post.body)
                .huddleText(Theme.Typography.message)
                .foregroundStyle(Theme.Colors.fg)
                .textSelection(.enabled)
            if let link = post.link {
                Link(destination: link) {
                    Text(link.absoluteString)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.actionText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Theme.Spacing.l)
        .huddleSurface(.card, radius: Theme.Radius.card)
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    @ViewBuilder
    private var avatar: some View {
        if let author = post.author {
            JIDAvatar(jid: author)
        } else {
            AvatarView(name: CommunityCopy.name(of: nil))
        }
    }
}
