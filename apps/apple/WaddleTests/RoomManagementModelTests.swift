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

    func testConfirmationsNameTheConversation() {
        let delete = RoomExitConfirmation(.delete, name: "design")
        XCTAssertEqual(delete?.title, "Delete “design”?")
        XCTAssertEqual(delete?.message, "The room and all its messages are removed for everyone. This can't be undone.")
        XCTAssertEqual(RoomExitConfirmation(.leaveGroup, name: "Weekend trip")?.confirmTitle, "Leave group chat")
        XCTAssertNil(RoomExitConfirmation(.editSettings, name: "design"))
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

    func testAGroupChatNamedAfterItsAddressHasNoNameToEdit() {
        XCTAssertEqual(GroupRenameEditor.currentName("g-7f3a2c", room: Self.group), "")
        XCTAssertEqual(GroupRenameEditor.currentName(" Weekend trip ", room: Self.group), "Weekend trip")
    }

    func testClearingAGroupChatNameCanBeSaved() {
        let editor = GroupRenameEditor(room: Self.group, channelName: "Weekend trip")
        XCTAssertFalse(editor.canSave)
        editor.name = ""
        XCTAssertTrue(editor.canSave)
    }

    func testAddPeopleNeverOffersSomeoneAlreadyInTheGroup() {
        let model = GroupInviteModel(room: Self.group, excluded: [SampleSession.jo])

        model.query = "jo@waddle.test"
        XCTAssertNil(model.typedAddress)
        XCTAssertEqual(model.typedMember, SampleSession.jo)
        XCTAssertFalse(model.canSend)

        model.query = "mika@waddle.test"
        XCTAssertEqual(model.invitees.map(\.jid), [SampleSession.mika])
    }

    func testAddPeopleInvitesEachPersonWithTheChosenHistory() async {
        let sample = SampleSession()
        let model = GroupInviteModel(
            room: Self.group,
            excluded: [],
            tokens: [RecipientToken(jid: SampleSession.mika, name: nil), RecipientToken(jid: SampleSession.theo, name: nil)]
        )
        model.history = .full

        let sent = await model.send(using: sample.coordinator)

        XCTAssertTrue(sent)
        XCTAssertEqual(sample.port.groupDMInvitations.map(\.invitee), [SampleSession.mika, SampleSession.theo])
        XCTAssertEqual(sample.port.groupDMInvitations.map(\.history), [.full, .full])
        XCTAssertEqual(model.tokens, [])
    }

    func testAFailedInvitationKeepsWhoIsLeftToInvite() async {
        let sample = SampleSession()
        sample.port.groupDMInvitationResult = .failure(.failed)
        let model = GroupInviteModel(
            room: Self.group, excluded: [], tokens: [RecipientToken(jid: SampleSession.mika, name: "Mika")]
        )

        let sent = await model.send(using: sample.coordinator)

        XCTAssertFalse(sent)
        XCTAssertEqual(model.tokens.map(\.jid), [SampleSession.mika])
        XCTAssertEqual(model.sendError, "Couldn't invite Mika. Try again.")
    }
}
