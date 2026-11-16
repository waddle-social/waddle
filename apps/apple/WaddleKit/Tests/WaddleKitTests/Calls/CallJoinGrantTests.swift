import Foundation
import Testing
import WaddleKit

@Suite("Call join grants") struct CallJoinGrantTests {
    private func join(_ token: String) -> CallJoin {
        CallJoin(url: "wss://livekit.waddle.test", room: "dm-room", identity: "alice@waddle.test/phone", token: token)
    }

    @Test func matchingGrantPasses() {
        let token = callToken(#"{"sub":"alice@waddle.test/phone","video":{"roomJoin":true,"room":"dm-room"}}"#)
        #expect(join(token).grantDefect == nil)
    }

    @Test(arguments: ["not-a-jwt", "a.!!!!.c", "a..c", "a.YQ=.c", "a.+w==.c", "a._w.c",
                      callToken("plain text"), callToken("[]"), callToken("null")])
    func malformedPayloadIsRejected(token: String) {
        #expect(join(token).grantDefect == .malformedToken)
    }

    @Test(arguments: [#"{}"#, #"{"video":null}"#, #"{"video":[]}"#, #"{"video":"grant"}"#])
    func missingGrantIsRejected(payload: String) {
        #expect(join(callToken(payload)).grantDefect == .missingGrant)
    }

    @Test(arguments: [#"{"video":{"roomJoin":false,"room":"dm-room"}}"#,
                      #"{"video":{"room":"dm-room"}}"#,
                      #"{"video":{"roomJoin":"TRUE","room":"dm-room"}}"#])
    func joinMustBeGranted(payload: String) {
        #expect(join(callToken(payload)).grantDefect == .joinNotGranted)
    }

    @Test(arguments: [#"{"video":{"roomJoin":true,"room":"  \n\t"}}"#,
                      #"{"video":{"roomJoin":true}}"#,
                      #"{"video":{"roomJoin":true,"room":null}}"#,
                      #"{"video":{"roomJoin":true,"room":[]}}"#])
    func missingRoomIsRejected(payload: String) {
        #expect(join(callToken(payload)).grantDefect == .missingRoom)
    }

    @Test(arguments: [#"{"video":{"roomJoin":true,"room":"other-room"}}"#,
                      #"{"sub":"mallory","video":{"roomJoin":true,"room":"other-room"}}"#])
    func roomMismatchPrecedesIdentityMismatch(payload: String) {
        #expect(join(callToken(payload)).grantDefect == .roomMismatch)
    }

    @Test(arguments: [#"{"sub":"mallory@waddle.test/x","video":{"roomJoin":true,"room":"dm-room"}}"#,
                      #"{"sub":"alice@waddle.test/tablet","video":{"roomJoin":true,"room":"dm-room"}}"#])
    func identityMismatchIsRejected(payload: String) {
        #expect(join(callToken(payload)).grantDefect == .identityMismatch)
    }

    @Test(arguments: [#"{"video":{"roomJoin":true,"room":"dm-room"}}"#,
                      #"{"sub":null,"video":{"roomJoin":true,"room":"dm-room"}}"#,
                      #"{"video":{"roomJoin":true,"room":"  dm-room  "}}"#])
    func subjectIsOptionalAndTheRoomIsTrimmed(payload: String) {
        #expect(join(callToken(payload)).grantDefect == nil)
    }
}
