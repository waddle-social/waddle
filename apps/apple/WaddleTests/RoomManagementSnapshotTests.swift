import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for room and group chat management: the
/// details screen as a room owner and as a group chat member, and each
/// sheet in its meaningful states. Forms and text fields only draw in a
/// real window, so these are screen snapshots, compared on the OS the
/// goldens were recorded on (see `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/RoomManagementSnapshotTests` and run the tests once to
/// record new ones (that run fails), then commit them.
@MainActor
final class RoomManagementSnapshotTests: XCTestCase {
    private static let group = BareJID(parsing: "g-7f3a2c@groups.waddle.test")!

    #if os(iOS)
    private static let details = CGSize(width: 393, height: 1020)
    private static let sheet = CGSize.phone
    #else
    /// The inspector at its ideal width.
    private static let details = CGSize(width: 360, height: 980)
    private static let sheet = CGSize(width: 640, height: 640)
    #endif

    // MARK: - Details

    func testDetailsForARoomOwner() async throws {
        let sample = SampleSession()
        sample.coordinator.directory.upsert(Channel(
            roomJID: SampleSession.design, name: "design",
            summary: "Critique, drafts and the identity work.", position: 3, spaceID: "hq"
        ))
        sample.join(SampleSession.design, as: .owner, role: .moderator)
        sample.occupy(SampleSession.design, nick: "Priya", jid: SampleSession.priya, affiliation: .admin)
        try await assertScreenSnapshot(of: sample.host(Self.details(.room(SampleSession.design))), size: Self.details)
    }

    func testDetailsForAGroupChatMember() async throws {
        let sample = SampleSession()
        sample.coordinator.directory.upsert(Channel(roomJID: Self.group, name: "Weekend trip", position: 0, isGroupDM: true))
        sample.join(Self.group, as: .member, role: .participant)
        sample.occupy(Self.group, nick: "Jo", jid: SampleSession.jo, affiliation: .member)
        sample.occupy(Self.group, nick: "Theo", jid: SampleSession.theo, affiliation: .owner)
        try await assertScreenSnapshot(of: sample.host(Self.details(.room(Self.group))), size: Self.details)
    }

    private static func details(_ conversation: ConversationID) -> some View {
        NavigationStack {
            ConversationDetailsView(conversation: conversation)
        }
    }

    // MARK: - Edit room

    private static let settings = RoomSettings(
        name: "design", summary: "Critique, drafts and the identity work.", pinPermission: .adminsOnly
    )

    /// The spinner turns on its own, so a little of the image may differ.
    func testEditRoomLoading() async throws {
        try await assertEditRoom(RoomSettingsEditor(room: SampleSession.design), precision: 0.995)
    }

    func testEditRoomFailedToLoad() async throws {
        try await assertEditRoom(RoomSettingsEditor(
            room: SampleSession.design,
            phase: .failed("The server took too long to answer. Try again.")
        ))
    }

    /// Nothing changed yet, so Save waits.
    func testEditRoomLoaded() async throws {
        try await assertEditRoom(RoomSettingsEditor(room: SampleSession.design, phase: .loaded(Self.settings)))
    }

    /// A server without the pin setting shows no picker; an edit enables Save.
    func testEditRoomEditedWithoutPinSetting() async throws {
        let editor = RoomSettingsEditor(
            room: SampleSession.design,
            phase: .loaded(RoomSettings(name: "design", summary: nil))
        )
        editor.name = "Design reviews"
        try await assertEditRoom(editor)
    }

    func testEditRoomWithoutAName() async throws {
        let editor = RoomSettingsEditor(room: SampleSession.design, phase: .loaded(Self.settings))
        editor.name = ""
        try await assertEditRoom(editor)
    }

    private func assertEditRoom(
        _ editor: RoomSettingsEditor, precision: Float = 0.9995, testName: String = #function
    ) async throws {
        let sample = SampleSession()
        try await assertScreenSnapshot(
            of: sample.host(EditRoomSheet(editor: editor)), size: Self.sheet, asSheet: true,
            precision: precision, testName: testName
        )
    }

    // MARK: - Group chat

    func testRenameGroupChat() async throws {
        let sample = SampleSession()
        let editor = GroupRenameEditor(room: Self.group, channelName: "Weekend trip")
        try await assertScreenSnapshot(of: sample.host(RenameGroupSheet(editor: editor)), size: Self.sheet, asSheet: true)
    }

    func testAddPeopleWithTwoPicked() async throws {
        let sample = SampleSession()
        let model = GroupAddPeopleModel(
            room: Self.group,
            excluded: [SampleSession.ana.jid, SampleSession.jo],
            tokens: [
                RecipientToken(jid: SampleSession.mika, name: "Mika Lindqvist"),
                RecipientToken(jid: SampleSession.priya, name: "Priya Shah"),
            ]
        )
        try await assertScreenSnapshot(of: sample.host(AddPeopleSheet(model: model)), size: Self.sheet, asSheet: true)
    }

    /// Search leaves out people already in the group chat (Jo).
    func testAddPeopleSearching() async throws {
        let sample = SampleSession()
        sample.port.userSearchResults["j"] = [
            UserSearchResult(jid: SampleSession.jo, displayName: "Jo Okafor"),
            UserSearchResult(jid: BareJID(parsing: "jun@waddle.test")!, displayName: "Jun Park"),
            UserSearchResult(jid: BareJID(parsing: "jamie@waddle.test")!, displayName: "Jamie Berg"),
        ]
        let model = GroupAddPeopleModel(room: Self.group, excluded: [SampleSession.ana.jid, SampleSession.jo])
        model.query = "j"
        try await assertScreenSnapshot(of: sample.host(AddPeopleSheet(model: model)), size: Self.sheet, asSheet: true)
    }
}

private extension SampleSession {
    /// Our own occupant in `room`.
    func join(_ room: BareJID, as affiliation: RoomAffiliation, role: RoomRole) {
        occupy(room, nick: Self.ana.nick, jid: Self.ana.jid, affiliation: affiliation, role: role, isSelf: true)
    }

    func occupy(
        _ room: BareJID, nick: String, jid: BareJID, affiliation: RoomAffiliation,
        role: RoomRole = .participant, isSelf: Bool = false
    ) {
        _ = coordinator.presence.apply(WirePresence(
            from: room.with(resource: nick)!,
            kind: .available,
            occupant: WirePresence.Occupant(
                affiliation: affiliation,
                role: role,
                realJID: jid.with(resource: "phone"),
                statusCodes: isSelf ? [110] : []
            )
        ))
    }
}
