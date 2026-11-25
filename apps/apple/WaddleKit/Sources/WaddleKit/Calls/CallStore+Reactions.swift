import Foundation

public extension CallStore {
    /// A reaction that arrived. Shown only when it is for the call in the
    /// slot and from someone in it (`CallReactionLedger.belongs`).
    func receiveReaction(_ reaction: InCallReaction, from: JID?, isGroupchat: Bool) {
        guard let from, CallReactionLedger.belongs(reaction, from: from, isGroupchat: isGroupchat, to: state) else { return }
        reactions.received(reaction.emoji, from: from)
    }

    /// Sends `emoji` to everyone in the call in the slot. A room hands ours
    /// back with everyone else's; a call of two does not, so there it is
    /// shown here as it is sent.
    @discardableResult
    func sendReaction(_ emoji: String) async -> Bool {
        guard CallReactionLedger.isOneEmoji(emoji), let destination = CallReactionLedger.destination(in: state) else { return false }
        guard await port.sendInCallReaction(emoji, to: destination) else { return false }
        // Still the same call: the send can outlast it.
        if case .device = destination, CallReactionLedger.destination(in: state) == destination {
            reactions.received(emoji, from: nil)
        }
        return true
    }
}
