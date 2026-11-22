import SwiftUI
import WaddleKit

/// A person's avatar with their availability dot at the bottom edge. With
/// no JID it draws their initials; with no availability, no dot.
struct PeerPresenceAvatar: View {
    let jid: BareJID?
    var name: String?
    let availability: Availability?
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        avatar
            .overlay(alignment: .bottomTrailing) {
                if let availability {
                    PresenceDot(availability: availability, size: dotSize)
                        .offset(x: dotSize * 0.25, y: dotSize * 0.25)
                }
            }
    }

    @ViewBuilder
    private var avatar: some View {
        if let jid {
            JIDAvatar(jid: jid, name: name, size: size)
        } else {
            AvatarView(name: name ?? "", size: size)
        }
    }

    private var dotSize: CGFloat {
        max(8, size * 0.3)
    }
}
