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
