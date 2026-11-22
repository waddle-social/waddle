import SwiftUI
import WaddleKit

/// Someone's published details (XEP-0292): name, nickname, pronouns, bio
/// and link. Only a web link can be opened.
struct PeerDetailsSection: View {
    let details: ProfileDetails

    var body: some View {
        Section {
            PeerDetailRow(title: "Name", value: details.fullName)
            PeerDetailRow(title: "Nickname", value: details.nickname)
            PeerDetailRow(title: "Pronouns", value: details.pronouns)
            if !details.bio.isEmpty {
                Text(details.bio)
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fg)
                    .textSelection(.enabled)
            }
            if let link = details.webLink {
                Link(destination: link) {
                    Label(details.link, systemImage: "link")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.actionText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            } else {
                PeerDetailRow(title: "Link", value: details.link)
            }
        } header: {
            FormSectionHeader("About")
        }
    }
}

private struct PeerDetailRow: View {
    let title: String
    let value: String

    var body: some View {
        if !value.isEmpty {
            LabeledContent {
                Text(value)
                    .foregroundStyle(Theme.Colors.fg)
                    .textSelection(.enabled)
            } label: {
                Text(title)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .huddleText(Theme.Typography.body)
        }
    }
}

extension View {
    /// Keeps `details` set to what `jid` has published, nil until it is
    /// known and when there is nothing to show.
    func peerDetails(of jid: BareJID?, session: SessionCoordinator, into details: Binding<ProfileDetails?>) -> some View {
        task(id: jid) {
            details.wrappedValue = nil
            guard let jid else { return }
            // A lookup for the person shown before can still answer after
            // this one; cancelling does not stop it.
            let loaded = await session.details(of: jid)
            guard !Task.isCancelled else { return }
            details.wrappedValue = loaded
        }
    }
}
