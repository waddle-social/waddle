import SwiftUI
import WaddleKit

/// Someone's published details (XEP-0292): name, nickname, pronouns, bio
/// and link. Only a web link can be opened.
struct PeerDetailsSection: View {
    let details: ProfileDetails

    var body: some View {
        Section("About") {
            PeerDetailRow(title: "Name", value: details.fullName)
            PeerDetailRow(title: "Nickname", value: details.nickname)
            PeerDetailRow(title: "Pronouns", value: details.pronouns)
            if !details.bio.isEmpty {
                Text(details.bio)
                    .textSelection(.enabled)
            }
            if let link = details.webLink {
                Link(destination: link) {
                    Label(details.link, systemImage: "link")
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            } else {
                PeerDetailRow(title: "Link", value: details.link)
            }
        }
    }
}

private struct PeerDetailRow: View {
    let title: String
    let value: String

    var body: some View {
        if !value.isEmpty {
            LabeledContent(title) {
                Text(value)
                    .textSelection(.enabled)
            }
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
            details.wrappedValue = await session.details(of: jid)
        }
    }
}
