import Foundation
@testable import WaddleKit

func jid(_ raw: String) -> JID {
    JID(parsing: raw)!
}

func bare(_ raw: String) -> BareJID {
    BareJID(parsing: raw)!
}

let me = AccountIdentity(jid: bare("alice@waddle.test"), nick: "alice")
let room = bare("general@muc.waddle.test")
let roomConversation = ConversationID.room(room)
let bob = bare("bob@waddle.test")
let bobConversation = ConversationID.direct(bob)

func date(_ seconds: TimeInterval) -> Date {
    Date(timeIntervalSince1970: 1_700_000_000 + seconds)
}

/// A reflected or archived room message from `nick`.
func roomMessage(
    _ body: String?,
    from nick: String,
    stanzaID: String,
    originID: String? = nil,
    at timestamp: Date? = nil,
    source: WireMessage.Source = .live
) -> WireMessage {
    WireMessage(
        source: source,
        type: .groupchat,
        from: room.with(resource: nick),
        to: JID(bare: me.jid, resource: "phone"),
        identity: MessageIdentity(
            messageID: originID,
            originID: originID,
            stanzaID: StanzaID(id: stanzaID, by: room),
            stanzaIDs: [StanzaID(id: stanzaID, by: room)]
        ),
        timestamp: timestamp,
        body: body
    )
}

/// A 1:1 message from `from` to `to`.
func directMessage(
    _ body: String?,
    from: JID,
    to: JID,
    id: String,
    archiveID: String? = nil,
    at timestamp: Date? = nil,
    source: WireMessage.Source = .live
) -> WireMessage {
    let stanzaIDs = archiveID.map { [StanzaID(id: $0, by: me.jid)] } ?? []
    return WireMessage(
        source: source,
        type: .chat,
        from: from,
        to: to,
        identity: MessageIdentity(messageID: id, originID: id, stanzaID: stanzaIDs.first, stanzaIDs: stanzaIDs),
        timestamp: timestamp,
        body: body
    )
}

func reaction(_ emojis: [String], to target: String, from nick: String, at timestamp: Date? = nil) -> WireMessage {
    var message = roomMessage(nil, from: nick, stanzaID: UUID().uuidString, at: timestamp)
    message.reaction = WireMessage.Reaction(targetID: target, emojis: emojis)
    return message
}
