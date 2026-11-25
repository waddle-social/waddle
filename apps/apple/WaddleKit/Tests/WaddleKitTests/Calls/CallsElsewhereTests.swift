import Testing

@testable import WaddleKit

/// Calls our other devices are in, as carbons of their XEP-0353 messages tell it.
struct CallsElsewhereTests {
    private let own = BareJID(parsing: "me@waddle.test")!
    private let device = JID(parsing: "me@waddle.test/mac")!
    private let phone = JID(parsing: "me@waddle.test/phone")!
    private let ada = JID(parsing: "ada@waddle.test/web")!
    private let audio = CallMedia(audio: true, video: false)
    private let video = CallMedia(audio: true, video: true)

    private func ledger(_ events: CallEvent...) -> CallsElsewhereLedger {
        var ledger = CallsElsewhereLedger()
        for event in events { ledger.apply(event, own: own, device: device) }
        return ledger
    }

    private func event(_ from: JID, to: JID? = nil, _ kind: CallEvent.Kind, sid: String = "s1") -> CallEvent {
        CallEvent(from: from, to: to, sid: sid, kind: kind)
    }

    @Test func aCallAnsweredOnOurPhoneIsLiveThere() {
        let ledger = ledger(event(ada, .propose(video)), event(phone, to: ada, .proceed))
        #expect(ledger.live == [CallElsewhere(sid: "s1", peer: ada.bare, media: video)])
        #expect(ledger.invitations.isEmpty)
    }

    @Test func anInvitationNobodyHasAnsweredIsNotACall() {
        #expect(ledger(event(ada, .propose(audio))).live.isEmpty)
    }

    /// We came online after the ring: the copy of the answer says whom it went to.
    @Test func anAnswerWhoseInvitationWeMissedStillNamesThePeer() {
        let ledger = ledger(event(phone, to: ada, .proceed))
        #expect(ledger.live == [CallElsewhere(sid: "s1", peer: ada.bare, media: nil)])
    }

    @Test func anAnswerThatSaysNothingOfWhomItWasForIsNoCall() {
        #expect(ledger(event(phone, .proceed)).live.isEmpty)
    }

    @Test func aCallOurPhonePlacedIsLiveOnceThePeerAnswers() {
        var ledger = ledger(event(phone, to: JID(parsing: "ada@waddle.test")!, .propose(audio)))
        #expect(ledger.live.isEmpty)
        ledger.apply(event(ada, to: phone, .proceed), own: own, device: device)
        #expect(ledger.live == [CallElsewhere(sid: "s1", peer: ada.bare, media: audio)])
    }

    @Test func ourOwnDevicesCallIsNotElsewhere() {
        // What this device sends is not copied back to it; were it to be, it is still its own call.
        let ledger = ledger(event(ada, .propose(audio)), event(device, to: ada, .proceed))
        #expect(ledger.live.isEmpty)
    }

    @Test func thePeersAnswerToThisDeviceIsThisDevicesCall() {
        let ledger = ledger(event(phone, to: JID(parsing: "ada@waddle.test")!, .propose(audio)), event(ada, to: device, .proceed))
        #expect(ledger.live.isEmpty)
    }

    @Test(arguments: [
        CallEvent.Kind.finish(reason: .success, migratedTo: nil),
        .reject(reason: nil, tieBreak: false),
        .retract(reason: nil, tieBreak: false),
    ])
    func eitherSideEndingItEndsIt(_ end: CallEvent.Kind) {
        for sender in [phone, ada] {
            var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
            ledger.apply(event(sender, end), own: own, device: device)
            #expect(ledger.live.isEmpty)
        }
    }

    @Test func anInvitationTakenBackIsForgotten() {
        let ledger = ledger(event(ada, .propose(audio)), event(ada, .retract(reason: nil, tieBreak: false)), event(phone, .proceed))
        #expect(ledger.live.isEmpty, "a late answer with no invitation and no recipient is no call")
    }

    @Test func aStrangerWhoLearnedTheSidCannotEndTheCall() {
        var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
        let stranger = JID(parsing: "eve@waddle.test/x")!
        ledger.apply(event(stranger, .finish(reason: .success, migratedTo: nil)), own: own, device: device)
        #expect(ledger.live.count == 1)
    }

    @Test func aRedeliveredInvitationForALiveCallChangesNothing() {
        var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
        ledger.apply(event(ada, .propose(audio)), own: own, device: device)
        ledger.apply(event(phone, to: ada, .proceed), own: own, device: device)
        #expect(ledger.live.count == 1)
        #expect(ledger.invitations.isEmpty)
    }

    @Test func twoCallsAreTwoCalls() {
        let grace = JID(parsing: "grace@waddle.test/web")!
        let ledger = ledger(
            event(ada, .propose(audio), sid: "a"), event(grace, .propose(video), sid: "g"),
            event(phone, to: ada, .proceed, sid: "a"), event(phone, to: grace, .proceed, sid: "g")
        )
        #expect(ledger.live.map(\.sid) == ["a", "g"])
    }

    @Test func onlyTheNewestInvitationsAreRemembered() {
        var ledger = CallsElsewhereLedger()
        for index in 0...CallsElsewhereLedger.invitationLimit {
            ledger.apply(event(ada, .propose(audio), sid: "s\(index)"), own: own, device: device)
        }
        #expect(ledger.invitations.count == CallsElsewhereLedger.invitationLimit)
        #expect(ledger.invitations["s0"] == nil)
    }

    @Test func aLostStreamForgetsEverything() {
        var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed), event(ada, .propose(audio), sid: "s2"))
        ledger.clear()
        #expect(ledger.live.isEmpty)
        #expect(ledger.invitations.isEmpty)
    }
}
