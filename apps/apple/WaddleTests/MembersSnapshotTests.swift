import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for a room's member list (Android `members`
/// frame): who is here now, then everyone with the bots. Compared on the
/// OS the goldens were recorded on (see `assertScreenSnapshot`).
@MainActor
final class MembersSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let size = CGSize.phone
    #else
    /// The inspector at its ideal width.
    private static let size = CGSize(width: 360, height: 980)
    #endif

    func testMembersHereNow() async throws {
        try await assertMembers(.here)
    }

    func testMembersEveryone() async throws {
        try await assertMembers(.everyone)
    }

    #if os(iOS)
    private static let sheet = CGSize.phone
    #else
    private static let sheet = CGSize(width: 420, height: 640)
    #endif

    /// Priya's profile opened from her card in #design (Android
    /// `conversation_author_profile`): her role there, then Message.
    func testProfileOfAMember() async throws {
        let sample = SampleSession()
        let room = SampleSession.design
        sample.occupy(room, nick: "Priya", jid: SampleSession.priya, affiliation: .admin, role: .moderator,
                      hats: [Hat(uri: "urn:waddle:hats:maintainer", title: "Maintainer")])
        let route = PeerProfileRoute(name: "Priya", jid: SampleSession.priya, room: room, nick: "Priya")
        try await assertScreenSnapshot(of: sample.host(PeerProfileSheet(profile: route)), size: Self.sheet, asSheet: true)
    }

    /// #design: four people here (us, Priya away with a hat, Jo, Theo on
    /// do not disturb), Mika online elsewhere, Sam offline, Mallory banned,
    /// and the Herald bot. The lists load before the screen draws.
    private func assertMembers(_ filter: RoomMembersView.Filter, testName: String = #function) async throws {
        let sample = SampleSession()
        let room = SampleSession.design
        let sam = BareJID(parsing: "sam@waddle.test")!
        sample.join(room, as: .owner, role: .moderator)
        sample.occupy(room, nick: "Priya", jid: SampleSession.priya, affiliation: .admin, role: .moderator, show: "away",
                      hats: [Hat(uri: "urn:waddle:hats:maintainer", title: "Maintainer")])
        sample.occupy(room, nick: "Jo", jid: SampleSession.jo, affiliation: .member)
        sample.occupy(room, nick: "Theo", jid: SampleSession.theo, affiliation: .member, show: "dnd")
        sample.port.roomMembers[room] = [
            RoomMember(jid: SampleSession.ana.jid, nick: nil, affiliation: .owner),
            RoomMember(jid: SampleSession.priya, nick: nil, affiliation: .admin),
            RoomMember(jid: SampleSession.jo, nick: nil, affiliation: .member),
            RoomMember(jid: SampleSession.theo, nick: nil, affiliation: .member),
            RoomMember(jid: SampleSession.mika, nick: "Mika", affiliation: .member),
            RoomMember(jid: sam, nick: "Sam", affiliation: .member),
            RoomMember(jid: BareJID(parsing: "mallory@waddle.test")!, nick: "Mallory", affiliation: .outcast),
        ]
        sample.port.roomBots[room] = [RoomBot(jid: BareJID(parsing: "herald@extensions.waddle.test")!, name: "Herald")]
        let model = RoomMembersModel(room: room)
        await model.loadMembers(using: sample.coordinator)
        await sample.coordinator.refreshRoomBots(in: room)
        try await assertScreenSnapshot(
            of: sample.host(NavigationStack { RoomMembersView(room: room, model: model, filter: filter) }),
            size: Self.size,
            testName: testName
        )
    }
}
