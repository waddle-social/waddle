import Testing
@testable import WaddleKit

@Suite("APNs push routing")
struct PushRouteTests {
    @Test func directMessageRoutesToThePeer() throws {
        let route = try #require(PushRoute(version: 1, node: "n1", conversation: "bob@waddle.test", notificationClass: "dm"))
        #expect(route.node == "n1")
        #expect(route.conversation == .direct(jid("bob@waddle.test").bare))
    }

    @Test func mentionClassesRouteToTheRoom() throws {
        for value in ["personal_mention", "channel_mention", "active_channel_mention", "notify_all"] {
            let route = try #require(PushRoute(version: 1, node: "n1", conversation: "general@muc.waddle.test", notificationClass: value))
            #expect(route.conversation.isRoom)
        }
        let dmMention = try #require(PushRoute(version: 1, node: "n1", conversation: "bob@waddle.test", notificationClass: "dm_mention"))
        #expect(!dmMention.conversation.isRoom)
    }

    @Test func unknownShapesAreIgnored() {
        #expect(PushRoute(version: 2, node: "n1", conversation: "bob@waddle.test", notificationClass: "dm") == nil)
        #expect(PushRoute(version: 1, node: "", conversation: "bob@waddle.test", notificationClass: "dm") == nil)
        #expect(PushRoute(version: 1, node: "n1", conversation: "not a jid@", notificationClass: "dm") == nil)
        #expect(PushRoute(version: 1, node: "n1", conversation: "bob@waddle.test", notificationClass: "gossip") == nil)
        #expect(PushRoute(version: nil, node: "n1", conversation: "bob@waddle.test", notificationClass: "dm") == nil)
    }
}
