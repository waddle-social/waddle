import Foundation
import WaddleKit
@testable import WaddleKitTestSupport

let foreignRoom = ConversationID.room(bare("dev@chat.remote.test"))
let foreignSender = bare("bob@remote.test")

let imageURL = "https://images.example/cat.png"
func mediaURL(_ path: String) -> URL { URL(string: "https://images.example/\(path)")! }

func roomRow(in conversation: ConversationID = roomConversation, sender: BareJID? = nil, isMine: Bool = false) -> TimelineItem {
    var message = roomMessage(imageURL, from: "bob", stanzaID: "stanza", originID: "message")
    message.from = conversation.jid.with(resource: "bob")
    message.authorRealJID = sender
    return TimelineItem(id: "message", conversation: conversation, isMine: isMine, message: message, body: imageURL)
}

func directRow(from sender: BareJID = bare("bob@remote.test")) -> TimelineItem {
    let message = directMessage(imageURL, from: JID(bare: sender, resource: nil)!, to: jid("alice@waddle.test/phone"), id: "message")
    return TimelineItem(id: "message", conversation: .direct(sender), isMine: false, message: message, body: imageURL)
}
