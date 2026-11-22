import Foundation
@testable import Waddle
@testable import WaddleKit

/// Conversations for the conversation screen goldens: the people and
/// messages the styleguide's room and DM frames use.
extension SampleSession {
    /// #general as the Web `room` frame shows it: grouped messages with
    /// reactions, an unread mention with a thread, and an edited message
    /// of our own.
    func seedGeneral() {
        let general = Self.general
        let messages: [WireMessage] = [
            roomMessage("Server tests are green on `main`. Tagging the RC after the huddle.", in: general, from: "mika", id: "m1", age: 2280),
            roomMessage("Changelog draft is in the PR description.", in: general, from: "mika", id: "m2", age: 2250),
            roomMessage("@Ana Lindqvist can you check room history on Android before we tag?", in: general, from: "jo", id: "m3", age: 1200),
            roomMessage("I'll take the Apple build. Ana, ping me if Android history looks off.", in: general, from: "priya", id: "m4", age: 1080),
            roomMessage("Thanks! Release notes go out with the tag.", in: general, from: "Ana Lindqvist", id: "m5", age: 0),
        ]
        for var message in messages {
            if message.body?.hasPrefix("@Ana Lindqvist") == true {
                message.references = [Reference(kind: .mention, uri: "xmpp:\(general)/Ana%20Lindqvist", begin: 0, end: 14)]
            }
            coordinator.timelines.ingest(message)
        }
        for (target, emoji, nicks) in [("m1", "🐧", ["jo", "priya", "theo", "Ana Lindqvist"]), ("m2", "🐧", ["jo", "priya", "theo", "Ana Lindqvist"]), ("m2", "👀", ["theo"])] {
            for nick in nicks {
                var message = roomMessage(nil, in: general, from: nick, id: "r-\(target)-\(nick)-\(emoji)", age: 0)
                message.reaction = WireMessage.Reaction(targetID: target, emojis: [emoji])
                coordinator.timelines.ingest(message)
            }
        }
    }

    private func roomMessage(_ body: String?, in room: BareJID, from nick: String, id: String, age: TimeInterval) -> WireMessage {
        WireMessage(
            source: .live,
            type: .groupchat,
            from: room.with(resource: nick),
            to: Self.ana.jid.with(resource: "phone"),
            identity: MessageIdentity(
                messageID: id,
                originID: id,
                stanzaID: StanzaID(id: id, by: room),
                stanzaIDs: [StanzaID(id: id, by: room)]
            ),
            timestamp: date(-age),
            body: body
        )
    }
}
