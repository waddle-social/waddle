import Foundation
import Testing

@testable import WaddleKit

struct CallAnchorCardTests {
    private let anchor = CallThreadAnchor(kind: .room, hasVideo: false)
    private let said = "ada started a call"

    private func live(
        _ people: [String] = ["Ada", "Linus"], video: Bool = false, ours: Bool = false, busy: Bool = false, canJoin: Bool = true
    ) -> CallAnchorCard.Live {
        CallAnchorCard.Live(people: people, hasVideo: video, isOurs: ours, isBusy: busy, canJoin: canJoin)
    }

    private func card(ended: CallThreadEnded? = nil, live: CallAnchorCard.Live? = nil, isNewest: Bool = true) -> CallAnchorCard {
        CallAnchorCard.make(anchor: anchor, said: said, ended: ended, live: live, isNewest: isNewest)
    }

    @Test func aCallGoingOnCanBeJoined() {
        let card = card(live: live())
        #expect(card.isLive)
        #expect(card.title == "Live call")
        #expect(card.summary == "Ada, Linus")
        #expect(card.headcount == 2)
        #expect(card.action == .join)
        #expect(card.spoken == "Join live call, 2 people: Ada, Linus")
    }

    @Test func theCallAsItIsNowDecidesTheMediaNotTheAnnouncement() {
        #expect(card(live: live(video: true)).title == "Live video call")
        #expect(card(live: live(video: true)).hasVideo)
    }

    @Test func manyPeopleAreCounted() {
        #expect(card(live: live(["Ada", "Linus", "Grace", "Margaret", "Alan"])).summary == "Ada, Linus, Grace +2")
    }

    @Test func aCallWeAreInHasNothingToJoin() {
        let card = card(live: live(ours: true, canJoin: false))
        #expect(card.isLive)
        #expect(card.action == nil)
        #expect(card.spoken == "Live call, 2 people: Ada, Linus")
    }

    @Test func inAnotherCallItSaysSoAndOfflineItSaysNothing() {
        #expect(card(live: live(busy: true, canJoin: false)).action == .busy)
        #expect(card(live: live(canJoin: false)).action == nil)
    }

    @Test func aCallTheRoomSaidIsOverShowsHowLongItRan() {
        let card = card(ended: CallThreadEnded(anchorID: "o1", duration: 300), live: live())
        #expect(!card.isLive)
        #expect(card.title == "Call ended · 5m")
        #expect(card.summary == said)
        #expect(card.action == nil)
        #expect(card.spoken == "Call ended · 5m. ada started a call")
    }

    @Test func aCallOverWithNoWordOfItsLength() {
        #expect(card(ended: CallThreadEnded(anchorID: "o1")).title == "Call ended")
        #expect(card().title == "Call ended", "no call in the room now: it is over, whether or not the room's word was read")
    }

    /// The web shows an old announcement as live again whenever the room has a later call.
    @Test func onlyTheLatestAnnouncementCanBeTheCallGoingOn() {
        let stale = card(live: live(), isNewest: false)
        #expect(!stale.isLive)
        #expect(stale.action == nil)
    }

    @Test(arguments: [(3723.0, "1h 2m"), (3600.0, "1h"), (300.0, "5m"), (359.0, "5m"), (42.0, "42s"), (0.0, "0s")])
    func durationsAreShort(_ seconds: Double, _ text: String) {
        #expect(CallAnchorCard.durationText(seconds) == text)
    }

    @Test func nobodyConnectedIsSaid() {
        let card = card(live: live([]))
        #expect(card.summary == "No one connected")
        #expect(card.spoken == "Join live call, no one connected")
    }
}
