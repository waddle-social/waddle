import XCTest
@testable import Waddle
@testable import WaddleKit

/// The decisions behind room and group chat management: what each action
/// opens, what a confirmed delete or leave does to navigation, and when
/// each sheet can save.
@MainActor
final class RoomManagementModelTests: XCTestCase {
    private static let group = BareJID(parsing: "g-7f3a2c@groups.waddle.test")!

    func testEachActionOpensItsSheetOrConfirmation() {
        let model = RoomManagementModel(room: SampleSession.design)

        model.select(.editSettings)
        XCTAssertEqual(model.sheet, .editRoom)
        model.select(.renameGroup)
        XCTAssertEqual(model.sheet, .renameGroup)
        model.select(.inviteToGroup)
        XCTAssertEqual(model.sheet, .addPeople)
        model.select(.delete)
        XCTAssertEqual(model.pendingExit, .delete)
    }

    func testDeletingTheRoomMovesNavigationOffIt() async {
        let sample = SampleSession()
        sample.navigation.open(.room(SampleSession.design))
        sample.navigation.inspector = .details
        let model = RoomManagementModel(room: SampleSession.design)
        model.select(.delete)

        await model.exit(.delete, using: sample.coordinator, navigation: sample.navigation)

        XCTAssertEqual(sample.port.roomDestructions.map(\.room), [SampleSession.design])
        XCTAssertNil(sample.navigation.selection)
        XCTAssertNil(sample.navigation.inspector)
        XCTAssertEqual(sample.navigation.homePath, [])
        XCTAssertNil(model.pendingExit)
        XCTAssertNil(model.exiting)
    }

    func testARefusedLeaveStaysAndSaysWhy() async {
        let sample = SampleSession()
        sample.port.groupDMLeaveResult = .failure(.notConnected)
        sample.navigation.open(.room(Self.group))
        let model = RoomManagementModel(room: Self.group)

        await model.exit(.leaveGroup, using: sample.coordinator, navigation: sample.navigation)

        XCTAssertEqual(sample.navigation.selection, .room(Self.group))
        XCTAssertEqual(model.exitFailure, RoomExitFailure(
            action: .leaveGroup, message: "You're offline. Try again when you're connected."
        ))
    }

    func testLeavingARoomMovesNavigationOffItAndListsItAsLeft() async {
        let sample = SampleSession()
        sample.navigation.open(.room(SampleSession.design))
        let model = RoomManagementModel(room: SampleSession.design)
        model.select(.leaveRoom)
        XCTAssertEqual(model.pendingExit, .leaveRoom)

        await model.exit(.leaveRoom, using: sample.coordinator, navigation: sample.navigation)

        XCTAssertEqual(sample.port.membershipWrites.map(\.membership), [.left])
        XCTAssertNil(sample.navigation.selection)
        XCTAssertEqual(sample.coordinator.directory.leftRooms.map(\.roomJID), [SampleSession.design])
        XCTAssertNil(model.exitFailure)
    }

    func testLeavingARoomSaysItCanBeJoinedAgain() {
        let leave = RoomExitConfirmation(.leaveRoom, name: "design")
        XCTAssertEqual(leave?.title, "Leave “design”?")
        XCTAssertEqual(
            leave?.message,
            "You'll stop getting its messages on all your devices. You can join again from Rooms you left."
        )
        XCTAssertEqual(leave?.confirmTitle, "Leave room")
    }

    func testJoiningALeftRoomOpensIt() async throws {
        let sample = SampleSession()
        try await sample.coordinator.leaveRoom(SampleSession.design)
        sample.port.topology = Topology(
            spaces: [], channels: sample.coordinator.directory.channels + sample.coordinator.directory.leftRooms)
        let model = LeftRoomsModel()

        let joined = await model.join(SampleSession.design, using: sample.coordinator, navigation: sample.navigation)

        XCTAssertTrue(joined)
        XCTAssertEqual(sample.navigation.selection, .room(SampleSession.design))
        XCTAssertTrue(sample.coordinator.directory.leftRooms.isEmpty)
        XCTAssertNil(model.failure)
    }

    func testJoiningARoomThatIsGoneSaysSo() async throws {
        let sample = SampleSession()
        try await sample.coordinator.leaveRoom(SampleSession.design)
        sample.port.topology = .empty
        let model = LeftRoomsModel()

        let joined = await model.join(SampleSession.design, using: sample.coordinator, navigation: sample.navigation)

        XCTAssertFalse(joined)
        XCTAssertEqual(model.failure, "That room no longer exists.")
        XCTAssertNil(sample.navigation.selection)
    }

    func testConfirmationsNameTheConversation() {
        let delete = RoomExitConfirmation(.delete, name: "design")
        XCTAssertEqual(delete?.title, "Delete “design”?")
        XCTAssertEqual(delete?.message, "The room is removed for everyone. This can't be undone.")
        XCTAssertEqual(RoomExitConfirmation(.leaveGroup, name: "Weekend trip")?.confirmTitle, "Leave group chat")
        XCTAssertNil(RoomExitConfirmation(.editSettings, name: "design"))
    }

    func testConfirmationsSayTheHuddleIsLeftToo() {
        XCTAssertEqual(
            RoomExitConfirmation(.leaveGroup, name: "Weekend trip", leavesHuddle: true)?.message,
            "You'll leave the huddle and stop getting its messages. To come back, someone in the group chat needs to add you again."
        )
        XCTAssertEqual(
            RoomExitConfirmation(.delete, name: "design", leavesHuddle: true)?.message,
            "The room is removed for everyone, and you leave the huddle. This can't be undone."
        )
    }

    func testEditRoomSavesOnlyANamedChange() {
        let editor = RoomSettingsEditor(
            room: SampleSession.design,
            phase: .loaded(RoomSettings(name: "design", summary: "Drafts", pinPermission: .adminsOnly))
        )
        XCTAssertFalse(editor.canSave)

        editor.pinPermission = .anyone
        XCTAssertTrue(editor.canSave)
        XCTAssertEqual(editor.patch, RoomSettingsPatch(pinPermission: .anyone))

        editor.name = "  "
        XCTAssertTrue(editor.isNameMissing)
        XCTAssertFalse(editor.canSave)
    }

    func testEditRoomCannotSaveBeforeItLoads() {
        let editor = RoomSettingsEditor(room: SampleSession.design)
        editor.name = "design reviews"
        XCTAssertFalse(editor.canSave)
    }

    func testRenameStartsFromTheNameTheDirectoryShows() {
        let editor = GroupRenameEditor(room: Self.group, channelName: "g-7f3a2c")
        XCTAssertEqual(editor.name, "g-7f3a2c")
        XCTAssertFalse(editor.canSave)
        editor.name = "  g-7f3a2c "
        XCTAssertFalse(editor.canSave)
        editor.name = ""
        XCTAssertTrue(editor.canSave)
    }

    func testAddPeopleNeverOffersSomeoneAlreadyInTheGroup() async {
        let sample = SampleSession()
        sample.port.userSearchResults["m"] = [
            UserSearchResult(jid: SampleSession.jo, displayName: "Jo"),
            UserSearchResult(jid: SampleSession.mika, displayName: "Mika"),
        ]
        let model = GroupAddPeopleModel(room: Self.group, excluded: [SampleSession.jo])

        await model.runSearch(for: "m", using: sample.coordinator)

        XCTAssertEqual(model.visibleSearch.results.map(\.jid), [SampleSession.mika])
    }

    /// Theo is a member but offline, so only the member list knows him.
    func testAddPeopleLeavesOutOfflineMembersOnceTheListLoads() async {
        let theo = RecipientToken(jid: SampleSession.theo, name: "Theo")
        let model = GroupAddPeopleModel(room: Self.group, excluded: [SampleSession.jo], tokens: [theo])

        await model.excludeMembers { [RoomMember(jid: SampleSession.theo, nick: "Theo", affiliation: .member)] }

        XCTAssertTrue(model.excluded.contains(SampleSession.theo))
        XCTAssertEqual(model.tokens, [])
        XCTAssertFalse(model.canAdd)
    }

    /// An owner reads the banned with the members; they are not in the group.
    func testAddPeopleStillOffersSomeoneBanned() async {
        let model = GroupAddPeopleModel(room: Self.group, excluded: [SampleSession.jo])

        await model.excludeMembers { [RoomMember(jid: SampleSession.theo, nick: "Theo", affiliation: .outcast)] }

        XCTAssertEqual(model.excluded, [SampleSession.jo])
    }

    func testGroupDetailsExcludesBannedMembers() {
        let members = [
            RoomMember(jid: SampleSession.theo, nick: "Theo", affiliation: .outcast),
            RoomMember(jid: SampleSession.jo, nick: "Jo", affiliation: .member),
        ]

        let groupMembers = RoomDetailsForm.groupMemberJIDs(
            present: [], members: members, account: SampleSession.ana.jid)

        XCTAssertEqual(groupMembers, [SampleSession.jo, SampleSession.ana.jid])
    }

    func testAddPeopleCarriesOnWhenTheMemberListFails() async {
        let model = GroupAddPeopleModel(room: Self.group, excluded: [SampleSession.jo])

        await model.excludeMembers { throw PortError.timeout }

        XCTAssertEqual(model.excluded, [SampleSession.jo])
    }

    func testAddPeopleAddsEachPersonWithTheChosenHistory() async {
        let sample = SampleSession()
        let model = GroupAddPeopleModel(
            room: Self.group,
            excluded: [],
            tokens: [RecipientToken(jid: SampleSession.mika, name: nil), RecipientToken(jid: SampleSession.theo, name: nil)]
        )
        model.history = .full

        let added = await model.add(using: sample.coordinator)

        XCTAssertTrue(added)
        XCTAssertEqual(sample.port.groupDMInvitations.map(\.invitee), [SampleSession.mika, SampleSession.theo])
        XCTAssertEqual(sample.port.groupDMInvitations.map(\.history), [.full, .full])
        XCTAssertEqual(model.tokens, [])
    }

    /// Theo turns out to be a member while Mika is still being added.
    func testAMemberListArrivingMidAddStopsThatPersonBeingAdded() async {
        let sample = SampleSession()
        sample.port.heldDirectoryVerbs = ["inviteToGroupDM"]
        let model = GroupAddPeopleModel(
            room: Self.group,
            excluded: [],
            tokens: [RecipientToken(jid: SampleSession.mika, name: nil), RecipientToken(jid: SampleSession.theo, name: nil)]
        )

        let adding = Task { await model.add(using: sample.coordinator) }
        while sample.port.heldDirectoryRequests.isEmpty { await Task.yield() }
        await model.excludeMembers { [RoomMember(jid: SampleSession.theo, nick: "Theo", affiliation: .member)] }
        sample.port.heldDirectoryVerbs = []
        sample.port.releaseDirectoryRequests()
        let added = await adding.value

        XCTAssertTrue(added)
        XCTAssertEqual(sample.port.groupDMInvitations.map(\.invitee), [SampleSession.mika])
    }

    func testAFailedAddKeepsWhoIsLeftToAdd() async {
        let sample = SampleSession()
        sample.port.groupDMInvitationResult = .failure(.failed)
        let model = GroupAddPeopleModel(
            room: Self.group, excluded: [], tokens: [RecipientToken(jid: SampleSession.mika, name: "Mika")]
        )

        let added = await model.add(using: sample.coordinator)

        XCTAssertFalse(added)
        XCTAssertEqual(model.tokens.map(\.jid), [SampleSession.mika])
        XCTAssertEqual(model.addError, "Couldn't add Mika. Try again.")
    }
}
