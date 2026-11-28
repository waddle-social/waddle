import SwiftUI
import WaddleKit

/// Avatar, name and address, with a tag on community owners. Opens their
/// profile.
struct AdminUserRow: View {
    @Environment(NavigationModel.self) private var navigation
    let user: AdminUser

    var body: some View {
        Button {
            navigation.showProfile(PeerProfileRoute(name: user.name, jid: user.jid, room: nil, nick: nil))
        } label: {
            NavigationRowChrome {
                HStack(spacing: Theme.Spacing.m) {
                    JIDAvatar(jid: user.jid, name: user.name)
                    VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                        Text(user.name)
                            .huddleText(Theme.Typography.author)
                            .foregroundStyle(Theme.Colors.fg)
                            .lineLimit(1)
                        Text(user.jid.description)
                            .huddleText(Theme.Typography.code.sized(WaddleTokens.FontSize.xs))
                            .foregroundStyle(Theme.Colors.fgMuted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if user.isOwner {
                        HuddleTag("Owner", tone: .gold)
                            .fixedSize()
                    }
                }
                .padding(.vertical, Theme.Spacing.m)
            }
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(user.isOwner ? "\(user.name), \(user.jid.description), owner" : "\(user.name), \(user.jid.description)"))
        .accessibilityHint(Text("Opens their profile"))
    }
}
