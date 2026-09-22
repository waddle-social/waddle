import SwiftUI
import WaddleKit

/// A person's avatar with their availability dot at the bottom edge.
struct PeerPresenceAvatar: View {
    let jid: BareJID
    var name: String?
    let availability: Availability
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        JIDAvatar(jid: jid, name: name, size: size)
            .overlay(alignment: .bottomTrailing) {
                PresenceDot(availability: availability, size: dotSize)
                    .offset(x: dotSize * 0.25, y: dotSize * 0.25)
            }
    }

    private var dotSize: CGFloat {
        max(8, size * 0.3)
    }
}
