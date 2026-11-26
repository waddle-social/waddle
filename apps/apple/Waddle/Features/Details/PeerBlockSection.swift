import SwiftUI
import WaddleKit

/// Block or unblock the person a conversation of two is with (XEP-0191).
/// Blocking asks first; unblocking does not, since it takes nothing away.
struct PeerBlockSection: View {
    @Environment(SessionCoordinator.self) private var session
    let peer: BareJID
    let name: String
    @State private var isConfirming = false
    @State private var isWorking = false
    @State private var didFail = false

    var body: some View {
        let covering = session.blockedEntry(covering: peer)
        let isBlocked = covering != nil
        // Blocked with their whole server: that entry is not this person's to lift from here.
        let own = covering.flatMap { $0.jid.bare == peer ? $0 : nil }
        Section {
            if let covering, own == nil {
                Text(BlockText.coveredNote(covering))
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
            } else {
                FormActionButton(
                    title: isBlocked ? "Unblock \(name)" : "Block \(name)",
                    symbol: isBlocked ? "hand.raised.slash" : "hand.raised",
                    isDestructive: !isBlocked
                ) {
                    if isBlocked { change(to: false) } else { isConfirming = true }
                }
                .disabled(isWorking || session.connection != .online)
                .confirmationDialog(BlockText.confirmTitle(name), isPresented: $isConfirming, titleVisibility: .visible) {
                    Button("Block", role: .destructive) { change(to: true) }
                    Button("Cancel", role: .cancel) {}
                } message: {
                    Text(BlockText.confirmMessage)
                }
            }
        } footer: {
            if didFail {
                FormSectionFooter(BlockText.failed)
            } else if own != nil {
                FormSectionFooter(BlockText.blockedNote)
            }
        }
    }

    private func change(to blocked: Bool) {
        isWorking = true
        didFail = false
        Task {
            do {
                if blocked {
                    try await session.block(peer)
                } else {
                    try await session.unblock(JID(bare: peer, resource: nil)!)
                }
            } catch {
                didFail = true
            }
            isWorking = false
        }
    }
}
