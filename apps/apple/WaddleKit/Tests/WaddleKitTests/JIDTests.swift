import Testing
@testable import WaddleKit

@Suite("RFC 7622 addresses")
struct JIDTests {
    @Test func parsesFullJID() {
        let parsed = JID(parsing: "Alice@Waddle.Test/Phone")
        #expect(parsed?.bare.localpart == "alice")
        #expect(parsed?.bare.domain == "waddle.test")
        #expect(parsed?.resource == "Phone")
    }

    @Test func resourceKeepsSlashesAndAts() {
        let parsed = JID(parsing: "room@muc.waddle.test/nick/with@odd")
        #expect(parsed?.bare == BareJID(parsing: "room@muc.waddle.test"))
        #expect(parsed?.resource == "nick/with@odd")
    }

    @Test func domainOnly() {
        #expect(BareJID(parsing: "waddle.test")?.localpart == nil)
    }

    @Test func rejectsInvalid() {
        #expect(JID(parsing: "") == nil)
        #expect(JID(parsing: "@waddle.test") == nil)
        #expect(JID(parsing: "alice@") == nil)
        #expect(JID(parsing: "alice@waddle.test/") == nil)
        #expect(BareJID(parsing: "alice@waddle.test/phone") == nil)
    }

    @Test func routesGroupchatByRoomAndNick() {
        let mine = me.route(from: jid("general@muc.waddle.test/alice"), to: nil, isGroupchat: true)
        #expect(mine == MessageRoute(conversation: roomConversation, isMine: true))
        let theirs = me.route(from: jid("general@muc.waddle.test/bob"), to: nil, isGroupchat: true)
        #expect(theirs?.isMine == false)
    }

    @Test func routesDirectToThePeer() {
        let incoming = me.route(from: jid("bob@waddle.test/laptop"), to: jid("alice@waddle.test/phone"), isGroupchat: false)
        #expect(incoming == MessageRoute(conversation: bobConversation, isMine: false))
        // A carbon of our own send from another device routes to the peer.
        let carbon = me.route(from: jid("alice@waddle.test/desktop"), to: jid("bob@waddle.test"), isGroupchat: false)
        #expect(carbon == MessageRoute(conversation: bobConversation, isMine: true))
    }

    @Test func mentionReferenceResolvesJID() {
        let reference = Reference(kind: .mention, uri: "xmpp:bob@waddle.test", begin: 0, end: 4)
        #expect(reference.mentionedJID?.bare == bob)
        let broadcast = Reference(kind: .mention, uri: "xmpp:@everyone", begin: 0, end: 9)
        #expect(broadcast.mentionedJID == nil)
    }
}
