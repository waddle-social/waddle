import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private let priya = bare("priya@waddle.test")
private let theo = bare("theo@waddle.test")
private let mika = bare("mika@waddle.test")
private let mallory = bare("mallory@waddle.test")

private func occupant(
    _ nick: String,
    _ availability: Availability = .available,
    affiliation: RoomAffiliation = .member,
    jid: BareJID? = nil,
    hats: [Hat] = []
) -> Occupant {
    Occupant(nick: nick, availability: availability, status: nil, affiliation: affiliation, role: .participant, realJID: jid, hats: hats)
}

@Suite("Room member cards")
struct MemberCardsTests {
    /// Android `MembersViewModel` order: available, then away, then do not
    /// disturb; within each, owners, admins, members, others; then name.
    @Test func peopleInTheRoomAreHereByPresenceThenRoleThenName() {
        let cards = MemberRoster.cards(
            present: [
                occupant("theo", .doNotDisturb, jid: theo),
                occupant("Priya", .away, affiliation: .admin, jid: priya),
                occupant("helper", hats: [Hat(uri: Hat.botURI, title: "Bot")]),
                occupant("zed", affiliation: .owner),
                occupant("ana", .chat),
                occupant("bo", .extendedAway),
            ],
            members: nil,
            contacts: [:]
        )
        #expect(cards.map(\.name) == ["zed", "ana", "Priya", "bo", "theo"])
        #expect(cards.map(\.place) == [.here, .here, .hereAway, .hereAway, .hereBusy])
        #expect(cards.allSatisfy { $0.isHere })
        #expect(cards[2].affiliation == .admin)
        #expect(cards[2].jid == priya)
        #expect(cards[2].nick == "Priya")
    }

    /// Everyone: affiliated people not in the room follow those here,
    /// those around first, then by affiliation and name. A member counts as
    /// here by real JID or by reserved nick. Online contacts are around;
    /// the rest, and the banned, are not here.
    @Test func membersNotInTheRoomFollowAndSayWhetherTheyAreAround() {
        let cards = MemberRoster.cards(
            present: [occupant("Priya", jid: priya), occupant("jo")],
            members: [
                RoomMember(jid: priya, nick: nil, affiliation: .admin),
                RoomMember(jid: bare("jo@waddle.test"), nick: "Jo", affiliation: .member),
                RoomMember(jid: theo, nick: nil, affiliation: .member),
                RoomMember(jid: mallory, nick: "Mallory", affiliation: .outcast),
                RoomMember(jid: mika, nick: "mika", affiliation: .owner),
            ],
            contacts: [
                theo: .init(availability: .away, status: nil, idleSince: nil),
                mika: .init(availability: .offline, status: nil, idleSince: nil),
            ]
        )
        #expect(cards.map(\.name) == ["jo", "Priya", "theo", "mika", "Mallory"])
        #expect(cards.map(\.place) == [.here, .here, .elsewhere, .notHere, .notHere])
        #expect(cards[2].availability == .away)
        #expect(cards[3].availability == .offline)
        #expect(cards[4].affiliation == .outcast)
        #expect(cards[3].nick == "mika")
        // A dot for those here or around; none for the rest.
        #expect(cards.map(\.shownAvailability) == [.available, .available, .away, nil, nil])
        // The banned are listed but are not members.
        #expect(MemberRoster.memberCount(cards) == 4)
    }
}
