import Foundation
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
    private let start = Date(timeIntervalSince1970: 1_800_000_000)

    private func ledger(_ events: CallEvent...) -> CallsElsewhereLedger {
        var ledger = CallsElsewhereLedger()
        for event in events { ledger.apply(event, own: own, device: device, at: start) }
        return ledger
    }

    private func event(_ from: JID, to: JID? = nil, _ kind: CallEvent.Kind, sid: String = "s1") -> CallEvent {
        CallEvent(from: from, to: to, sid: sid, kind: kind)
    }

    @Test func aCallAnsweredOnOurPhoneIsLiveThere() {
        let ledger = ledger(event(ada, .propose(video)), event(phone, to: ada, .proceed))
        #expect(ledger.live == [CallElsewhere(sid: "s1", peer: ada.bare, media: video, since: start)])
        #expect(ledger.invitations.isEmpty)
    }

    @Test func anInvitationNobodyHasAnsweredIsNotACall() {
        #expect(ledger(event(ada, .propose(audio))).live.isEmpty)
    }

    /// We came online after the ring: the copy of the answer says whom it went to.
    @Test func anAnswerWhoseInvitationWeMissedStillNamesThePeer() {
        let ledger = ledger(event(phone, to: ada, .proceed))
        #expect(ledger.live == [CallElsewhere(sid: "s1", peer: ada.bare, media: nil, since: start)])
    }

    @Test func anAnswerThatSaysNothingOfWhomItWasForIsNoCall() {
        #expect(ledger(event(phone, .proceed)).live.isEmpty)
    }

    @Test func aCallOurPhonePlacedIsLiveOnceThePeerAnswers() {
        var ledger = ledger(event(phone, to: JID(parsing: "ada@waddle.test")!, .propose(audio)))
        #expect(ledger.live.isEmpty)
        ledger.apply(event(ada, to: phone, .proceed), own: own, device: device, at: start)
        #expect(ledger.live == [CallElsewhere(sid: "s1", peer: ada.bare, media: audio, since: start)])
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

    @Test func eitherSideFinishingEndsIt() {
        for sender in [phone, ada] {
            var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
            ledger.apply(event(sender, .finish(reason: .success, migratedTo: nil)), own: own, device: device, at: start)
            #expect(ledger.live.isEmpty)
        }
    }

    /// The ring went to two devices: one answers, and the other declines the ring it still shows a moment later.
    @Test func aLateDeclineFromADeviceNotInTheCallDoesNotEndIt() {
        let tablet = JID(parsing: "me@waddle.test/tablet")!
        var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
        ledger.apply(event(tablet, to: ada, .reject(reason: nil, tieBreak: false)), own: own, device: device, at: start)
        ledger.apply(event(ada, .retract(reason: nil, tieBreak: false)), own: own, device: device, at: start)
        #expect(ledger.live.count == 1)
    }

    @Test func anInvitationDeclinedIsForgotten() {
        let ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .reject(reason: nil, tieBreak: false)))
        #expect(ledger.invitations.isEmpty)
    }

    /// A device that dies mid-call never says the call is over.
    @Test func aCallIsNotShownForMoreThanADay() {
        var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
        let call = ledger.live[0]
        #expect(call.isShown(at: start.addingTimeInterval(CallElsewhere.longestShown - 1)))
        #expect(!call.isShown(at: start.addingTimeInterval(CallElsewhere.longestShown)))
        // And it is dropped for good when anything else happens after that.
        ledger.apply(event(ada, .propose(audio), sid: "s2"), own: own, device: device, at: start.addingTimeInterval(CallElsewhere.longestShown))
        #expect(ledger.live.isEmpty)
    }

    @Test func anInvitationTakenBackIsForgotten() {
        let ledger = ledger(event(ada, .propose(audio)), event(ada, .retract(reason: nil, tieBreak: false)), event(phone, .proceed))
        #expect(ledger.live.isEmpty, "a late answer with no invitation and no recipient is no call")
    }

    @Test func aStrangerWhoLearnedTheSidCannotEndTheCall() {
        var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
        let stranger = JID(parsing: "eve@waddle.test/x")!
        ledger.apply(event(stranger, .finish(reason: .success, migratedTo: nil)), own: own, device: device, at: start)
        #expect(ledger.live.count == 1)
    }

    @Test func aRedeliveredInvitationForALiveCallChangesNothing() {
        var ledger = ledger(event(ada, .propose(audio)), event(phone, to: ada, .proceed))
        ledger.apply(event(ada, .propose(audio)), own: own, device: device, at: start)
        ledger.apply(event(phone, to: ada, .proceed), own: own, device: device, at: start)
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
            ledger.apply(event(ada, .propose(audio), sid: "s\(index)"), own: own, device: device, at: start)
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
