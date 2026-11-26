import Foundation
import Testing

@testable import WaddleKit

struct LiveCallsTests {
    private func bare(_ raw: String) -> BareJID { BareJID(parsing: raw)! }

    private func people(_ names: String...) -> [GroupCallRosterRow] {
        names.map { GroupCallRosterRow(nick: $0, jid: BareJID(parsing: "\($0.lowercased())@waddle.test")) }
    }

    private func room(_ name: String, _ people: [GroupCallRosterRow], video: Bool = false, canJoin: Bool = true) -> LiveCalls.Room {
        LiveCalls.Room(
            room: bare("\(name.lowercased().replacingOccurrences(of: " ", with: "-"))@rooms.waddle.test"), title: name, people: people, hasVideo: video,
            canJoin: canJoin
        )
    }

    @Test func aRoomWithACallIsListedWithAWayIn() {
        let calls = LiveCalls.list([room("Design", people("Ada", "Linus", "Grace"))], state: .idle)
        #expect(calls.map(\.title) == ["Design"])
        #expect(calls[0].action == .join)
        #expect(calls[0].meta == "Group call · 3 people")
        #expect(calls[0].headcount == "3 people")
        #expect(calls[0].preview() == "Ada, Linus and 1 more")
        #expect(calls[0].spoken == "Join Design, group call, 3 people: Ada, Linus and 1 more")
    }

    @Test func oneAloneInAVideoCall() {
        let call = LiveCalls.list([room("Design", people("Ada"), video: true)], state: .idle)[0]
        #expect(call.meta == "Group video call · 1 person")
        #expect(call.preview() == "Ada")
    }

    @Test func aRoomNobodyIsInHasNoCall() {
        #expect(LiveCalls.list([room("Design", [])], state: .idle).isEmpty)
    }

    @Test func theCallWeAreInHasItsOwnCard() {
        let design = room("Design", people("Ada"))
        let joining = CallState.joiningGroup(
            GroupCallAttempt(room: design.room, sid: "g1", media: CallMedia(audio: true, video: false), nick: "me")
        )
        #expect(LiveCalls.list([design], state: joining).isEmpty)
    }

    @Test func aCallThatCannotBeJoinedNowOpensItsRoom() {
        let calls = LiveCalls.list([room("Design", people("Ada"), canJoin: false)], state: .idle)
        #expect(calls[0].action == .open)
        #expect(calls[0].spoken.hasPrefix("Open Design"))
    }

    @Test func callsWeCanJoinComeFirstThenByName() {
        let calls = LiveCalls.list(
            [
                room("Zebra", people("Ada")),
                room("apple", people("Ada"), canJoin: false),
                room("Room 10", people("Ada")),
                room("Room 2", people("Ada")),
            ],
            state: .idle
        )
        #expect(calls.map(\.title) == ["Room 2", "Room 10", "Zebra", "apple"])
    }

    @Test func ourOtherDeviceInTheCallIsYou() {
        var rows = people("Me", "Ada")
        rows[0].isSelf = true
        let call = LiveCalls.list([room("Design", rows)], state: .idle)[0]
        #expect(call.preview() == "You and Ada")
        #expect(call.spoken == "Join Design, group call, 2 people: You and Ada")
    }

    @Test func aCallOnAnotherOfOurDevicesCanOnlyBeOpened() {
        let ada = bare("ada@waddle.test")
        let call = CallElsewhere(sid: "s1", peer: ada, media: CallMedia(audio: true, video: false), since: Date())
        let calls = LiveCalls.list([], elsewhere: [(call, "Ada")], state: .idle)
        #expect(calls.count == 1)
        #expect(calls[0].conversation == .direct(ada))
        #expect(calls[0].action == .open)
        #expect(calls[0].meta == "Voice call · On another device")
        #expect(calls[0].headcount == "On another device")
        #expect(calls[0].spoken == "Open Ada, voice call, live on another of your devices")
        #expect(calls[0].people.map(\.jid) == [ada])
    }

    @Test func aCallWhoseRingMerelyEndedHereIsStillLiveElsewhere() {
        let call = CallElsewhere(sid: "s1", peer: bare("ada@waddle.test"), media: nil, since: Date())
        #expect(LiveCalls.list([], elsewhere: [(call, "Ada")], state: .ended(sid: "s1", reason: .missed)).count == 1)
    }

    @Test func aCallTooOldToBelieveIsNotListed() {
        let now = Date()
        let call = CallElsewhere(sid: "s1", peer: bare("ada@waddle.test"), media: nil, since: now.addingTimeInterval(-CallElsewhere.longestShown))
        #expect(LiveCalls.list([], elsewhere: [(call, "Ada")], state: .idle, now: now).isEmpty)
    }

    @Test func theListNextChangesWhenItsOldestCallRunsOut() {
        let now = Date()
        let ada = bare("ada@waddle.test")
        let older = CallElsewhere(sid: "a", peer: ada, media: nil, since: now.addingTimeInterval(-3600))
        let newer = CallElsewhere(sid: "b", peer: ada, media: nil, since: now)
        let gone = CallElsewhere(sid: "c", peer: ada, media: nil, since: now.addingTimeInterval(-CallElsewhere.longestShown - 1))
        #expect(LiveCalls.nextExpiry(of: [newer, older, gone], after: now) == older.shownUntil)
        #expect(LiveCalls.nextExpiry(of: [gone], after: now) == nil)
        #expect(LiveCalls.nextExpiry(of: [], after: now) == nil)
        // And at that moment the call is no longer listed.
        #expect(LiveCalls.list([], elsewhere: [(older, "Ada")], state: .idle, now: older.shownUntil).isEmpty)
    }

    @Test func aCallWhoseInvitationWeMissedIsJustACall() {
        let call = CallElsewhere(sid: "s1", peer: bare("ada@waddle.test"), media: nil, since: Date())
        let live = LiveCalls.list([], elsewhere: [(call, "Ada")], state: .idle)[0]
        #expect(live.meta == "Call · On another device")
        #expect(LiveCall(elsewhere: CallElsewhere(sid: "s2", peer: call.peer, media: CallMedia(audio: true, video: true)), name: "Ada").meta
            == "Video call · On another device")
    }

    @Test func roomsWeCanJoinComeBeforeCallsWeCanOnlyOpen() {
        let call = CallElsewhere(sid: "s1", peer: bare("ada@waddle.test"), media: nil, since: Date())
        let calls = LiveCalls.list([room("Zebra", people("Linus"))], elsewhere: [(call, "Ada")], state: .idle)
        #expect(calls.map(\.title) == ["Zebra", "Ada"])
        #expect(Set(calls.map(\.id)).count == 2)
    }

    @Test func aRoomAndAPersonWithOneAddressAreTwoCalls() {
        let call = CallElsewhere(sid: "s1", peer: bare("design@rooms.waddle.test"), media: nil, since: Date())
        let calls = LiveCalls.list([room("Design", people("Linus"))], elsewhere: [(call, "Design")], state: .idle)
        #expect(Set(calls.map(\.id)).count == 2)
    }

    @Test func theHeadingCountsAndACallStartingIsSaid() {
        #expect(LiveCalls.heading(count: 2) == "Live now · 2")
        #expect(LiveCalls.announcement(count: 1, besidesOurs: false) == "1 active call.")
        #expect(LiveCalls.announcement(count: 3, besidesOurs: true) == "3 other active calls.")
    }

    private func change(_ count: Int, own: String? = nil, online: Bool = true) -> LiveCalls.Change {
        LiveCalls.Change(count: count, ownCall: own, isOnline: online)
    }

    @Test func aCallStartingIsNews() {
        #expect(LiveCalls.isNews(before: change(0), after: change(1)))
        #expect(LiveCalls.isNews(before: change(1, own: "g1"), after: change(2, own: "g1")))
    }

    @Test func aCallEndingIsNot() {
        #expect(!LiveCalls.isNews(before: change(2), after: change(1)))
    }

    /// Joining takes the call out of the list and leaving puts it back: the call itself says both.
    @Test func ourOwnComingAndGoingIsNot() {
        #expect(!LiveCalls.isNews(before: change(1), after: change(0, own: "g1")))
        #expect(!LiveCalls.isNews(before: change(0, own: "g1"), after: change(1)))
    }

    /// Presence is cleared when the connection drops and comes back room by room.
    @Test func roomsComingBackAfterAReconnectAreNot() {
        #expect(!LiveCalls.isNews(before: change(0, online: false), after: change(1)))
        #expect(!LiveCalls.isNews(before: change(1), after: change(2, online: false)))
    }
}
