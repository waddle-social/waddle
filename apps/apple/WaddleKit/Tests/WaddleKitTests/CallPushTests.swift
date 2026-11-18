import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@Suite("VoIP call push parsing")
struct CallPushTests {
    @Test func parsesCall() throws {
        let push = try #require(CallPush(userInfo: payload()))
        #expect(push.conversation == jid("bob@waddle.test").bare)
        #expect(push.sid == "call-1")
        #expect(!push.isVideo)
        #expect(push.expires == Date(timeIntervalSince1970: 2_000_000_000))
        #expect(push.node == "voip-node")
    }

    @Test func parsesVideoCall() throws {
        let push = try #require(CallPush(userInfo: payload(call: ["video": true])))
        #expect(push.isVideo)
    }

    @Test func acceptsJSONNumbersAndBooleans() throws {
        let data = try JSONSerialization.data(withJSONObject: payload())
        let decoded = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(CallPush(userInfo: decoded) == CallPush(userInfo: payload()))
        let fractional = try #require(CallPush(userInfo: payload(call: ["expires": 2_000_000_000.5])))
        #expect(fractional.expires == Date(timeIntervalSince1970: 2_000_000_000.5))
    }

    @Test func rejectsUnknownVersionsAndClasses() {
        for version in [2, true, "1", 1.5] as [Any] {
            #expect(CallPush(userInfo: payload(waddle: ["v": version])) == nil)
        }
        #expect(CallPush(userInfo: payload(waddle: ["class": "dm"])) == nil)
    }

    @Test func rejectsMissingFields() {
        #expect(CallPush(userInfo: [:]) == nil)
        for key in ["v", "class", "conversation", "item", "node", "call"] {
            #expect(CallPush(userInfo: payload(waddle: [key: nil])) == nil)
        }
        for key in ["sid", "video", "expires"] {
            #expect(CallPush(userInfo: payload(call: [key: nil])) == nil)
        }
    }

    @Test func rejectsEmptyIdentifiersAndInvalidConversation() {
        #expect(CallPush(userInfo: payload(call: ["sid": ""])) == nil)
        #expect(CallPush(userInfo: payload(waddle: ["node": ""])) == nil)
        #expect(CallPush(userInfo: payload(waddle: ["item": ""])) == nil)
        #expect(CallPush(userInfo: payload(waddle: ["conversation": "not a jid@"])) == nil)
        #expect(CallPush(userInfo: payload(waddle: ["conversation": "bob@waddle.test/phone"])) == nil)
    }

    @Test func ignoresLegacySender() throws {
        let expected = try #require(CallPush(userInfo: payload()))
        for sender in ["bob@waddle.test/phone", "alice@waddle.test/phone", "not a jid@", 42, NSNull()] as [Any] {
            #expect(CallPush(userInfo: payload(call: ["from": sender])) == expected)
        }
    }

    @Test func rejectsInvalidExpiresAndVideo() {
        for expires in ["2000000000", true, Double.infinity, Double.nan, NSNull()] as [Any] {
            #expect(CallPush(userInfo: payload(call: ["expires": expires])) == nil)
        }
        for video in [0, 1, "false", NSNull()] as [Any] {
            #expect(CallPush(userInfo: payload(call: ["video": video])) == nil)
        }
    }

    @Test func expiresAtDeadline() throws {
        let push = try #require(CallPush(userInfo: payload()))
        #expect(!push.isExpired(at: push.expires.addingTimeInterval(-0.001)))
        #expect(push.isExpired(at: push.expires))
        #expect(push.isExpired(at: push.expires.addingTimeInterval(1)))
    }

    private func payload(waddle changes: [String: Any?] = [:], call callChanges: [String: Any?] = [:]) -> [AnyHashable: Any] {
        var call: [String: Any] = [
            "sid": "call-1", "video": false, "expires": 2_000_000_000,
        ]
        for (key, value) in callChanges { call[key] = value }
        var waddle: [String: Any] = [
            "v": 1, "class": "call", "conversation": "bob@waddle.test", "item": "stanza-1",
            "node": "voip-node", "call": call,
        ]
        for (key, value) in changes { waddle[key] = value }
        return ["waddle": waddle]
    }
}
