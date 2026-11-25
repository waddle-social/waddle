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
        #expect(calls[0].preview() == "Ada, Linus +1")
        #expect(calls[0].spoken == "Join Design, Group call · 3 people, Ada, Linus +1 in call")
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

    @Test func theHeadingCountsAndTheChangeIsSaid() {
        #expect(LiveCalls.heading(count: 2) == "Live now · 2")
        #expect(LiveCalls.announcement(count: 0) == "No other active calls.")
        #expect(LiveCalls.announcement(count: 1) == "1 other active call.")
        #expect(LiveCalls.announcement(count: 3) == "3 other active calls.")
    }
}
