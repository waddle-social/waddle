import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

struct RoomLinksTests {
    private let room = bare("ops@rooms.waddle.test")
    private let federation = bare("ops@rooms.other.test")
    private let uri = "xmpp:ops@rooms.waddle.test?join"

    @Test func filtersAndRanksRoomsByNameOrAddressInDirectoryOrder() {
        let channels = [
            Channel(roomJID: bare("devops@rooms.waddle.test"), name: "DevOps"),
            Channel(roomJID: federation, name: "Other"),
            Channel(roomJID: bare("hidden@rooms.waddle.test"), name: "Ops", isGroupDM: true),
            Channel(roomJID: bare("left@rooms.waddle.test"), name: "Ops", hasLeft: true),
            Channel(roomJID: bare("lobby@rooms.waddle.test"), name: "Ops", autojoin: false),
            Channel(roomJID: room, name: ""),
        ]
        let candidates = RoomLinks.candidates(from: channels)
        #expect(candidates.map(\.label) == ["#DevOps", "#Other", "#Ops", "#ops"])
        #expect(RoomLinks.search(candidates, for: "OPS").map(\.room) == [
            federation, bare("lobby@rooms.waddle.test"), room, bare("devops@rooms.waddle.test"),
        ])
        #expect(RoomLinks.search(candidates, for: "") == candidates)
        #expect(RoomLinks.availableRoom(federation, in: channels) == federation)
        #expect(RoomLinks.availableRoom(bare("ops@elsewhere.test"), in: channels) == nil)
        #expect(RoomLinks.availableRoom(bare("left@rooms.waddle.test"), in: channels) == nil)
        #expect(RoomLinks.availableRoom(bare("hidden@rooms.waddle.test"), in: channels) == nil)

        let many = (1...9).map { Channel(roomJID: bare("room\($0)@rooms.waddle.test"), name: "Room \($0)") }
        #expect(RoomLinks.search(RoomLinks.candidates(from: many), for: "").map(\.label) == (1...8).map { "#Room \($0)" })
    }

    @Test func uriMatchesEncodeURIComponentAndParsesAsABareAddress() {
        let address = bare("é+%?!*=#@rooms.example")
        let escaped = "xmpp:%C3%A9%2B%25%3F!*%3D%23@rooms.example?join"
        #expect(RoomLinks.joinURI(for: address) == escaped)
        #expect(RoomLinks.room(fromJoinURI: escaped) == address)
        #expect(RoomLinks.room(fromJoinURI: "XMPP:ops@ROOMS.WADDLE.TEST?JOIN") == room)
        #expect(RoomLinks.room(fromJoinURI: "xmpp:ops@[::1]?join") == bare("ops@[::1]"))
    }

    @Test(arguments: [
        "https:ops@rooms.example?join", "xmpp://ops@rooms.example?join", "xmpp:rooms.example?join",
        "xmpp:ops@rooms.example", "xmpp:ops@rooms.example?message", "xmpp:ops@rooms.example?join;x=y",
        "xmpp:ops@rooms.example?join#foo", "xmpp:ops@rooms.example?join\n", "xmpp:ops@rooms.example/else?join",
        "xmpp:ops@rooms.example:443?join", "xmpp:ops@..example?join", "xmpp:ops@[:::]?join",
        "xmpp:a%2Fb@rooms.example?join", "xmpp:a%40b@rooms.example?join", "xmpp:a%20b@rooms.example?join",
        "xmpp:a%00b@rooms.example?join", "xmpp:a%26%CC%81b@rooms.example?join", "xmpp:%FF@rooms.example?join",
        "xmpp:%GG@rooms.example?join", "xmpp:é@rooms.example?join",
    ])
    func rejectsHostileOrMalformedUris(uri: String) {
        #expect(RoomLinks.room(fromJoinURI: uri) == nil)
    }

    @Test func onlyDataReferencesResolveRoomTargets() {
        #expect(RoomLinks.room(from: Reference(kind: .data, uri: uri, begin: 1, end: 5)) == room)
        #expect(RoomLinks.room(from: Reference(kind: .mention, uri: uri, begin: 1, end: 5)) == nil)
        #expect(RoomLinks.room(from: Reference(kind: .data, uri: uri, begin: 5, end: 5)) == nil)
        #expect(RoomLinks.room(from: Reference(kind: .data, uri: "https://example.org", begin: 1, end: 5)) == nil)
    }

    @Test func onlyThePickedOccurrenceSurvivesEditsBeforeAndAfterIt() throws {
        let original = "#ops #o"
        let model = presentation(original)
        guard case let .pick(pick) = model.handle(.tab, in: original) else {
            Issue.record("Tab should pick the room")
            return
        }
        guard case let .roomLink(link) = pick.reference else {
            Issue.record("Picking a room should record the occurrence")
            return
        }
        #expect(pick.edit.text == "#ops #ops ")
        #expect(link.range == 5..<9)

        let inserted = "#ops " + pick.edit.text
        let moved = RoomLinks.relocate([link], from: pick.edit.text, to: inserted, replacing: 0..<0)
        let final = inserted + "and #ops"
        let located = RoomLinks.relocate(moved, from: inserted, to: final, replacing: nil)
        #expect(MessageComposer.compose(Draft(text: final, references: .init(roomLinks: located)))?.options.references == [
            Reference(kind: .data, uri: uri, begin: 10, end: 14),
        ])

        let edited = "#ops #ops #no and #ops"
        #expect(RoomLinks.relocate(located, from: final, to: edited, replacing: 10..<14).isEmpty)
        #expect(RoomLinks.relocate(located, from: final, to: final, replacing: 10..<14).isEmpty)
        #expect(MessageComposer.compose(Draft(text: final))?.options.references.isEmpty == true)
    }

    @Test(arguments: [
        ("#ops-extra", 0..<4, "#ops-extra", false),
        ("#ops", 0..<4, "#ops", false),
        ("-extra", 4..<4, "#ops-extra", true),
    ])
    func replacingTheSelectionDropsOnlyLinksItEdits(
        insertion: String, selection: Range<Int>, body: String, keepsLink: Bool
    ) throws {
        let pick = try #require(presentation("#o").pickForSubmit(in: "#o"))
        var references = RecordedDraftReferences()
        references.append(try #require(pick.reference))
        let change = ComposerTextReplacement.replacingSelection(with: insertion, in: pick.edit.text, selection: selection)
        references.relocate(from: pick.edit.text, applying: change.replacement)
        let draft = Draft(text: change.edit.text, references: references.located(in: change.edit.text))
        let composed = try #require(MessageComposer.compose(draft))
        #expect(composed.body == body)
        #expect(composed.options.references == (keepsLink ? [Reference(kind: .data, uri: uri, begin: 0, end: 4)] : []))
    }

    @Test(arguments: [nil, 0..<13] as [Range<Int>?])
    func toolbarBoldPreservesAPickedLabelThatEndsInBoldMarkers(selection: Range<Int>?) throws {
        let label = "#Team **Ops**"
        let pick = try #require(presentation("#t", roomName: "Team **Ops**").pickForSubmit(in: "#t"))
        var references = RecordedDraftReferences()
        references.append(try #require(pick.reference))
        let edit = ComposerFormatting.apply(.bold, to: pick.edit.text, selection: selection, roomLinks: references.roomLinks)
        references.relocate(from: pick.edit.text, applying: edit.replacement)
        #expect(references.roomLinks == [RoomLinkDraft(room: room, range: 2..<15)])
        let draft = Draft(text: edit.text, references: references.located(in: edit.text))
        let composed = try #require(MessageComposer.compose(draft))
        #expect(composed.body == label)
        #expect(composed.options.markupSpans == [MarkupSpan(kind: .bold, start: 0, end: 13)])
        #expect(composed.options.references == [Reference(kind: .data, uri: uri, begin: 0, end: 13)])
    }

    private func presentation(
        _ text: String, roomName: String = "ops"
    ) -> ComposerSuggestionsModel {
        ComposerSuggestionsModel.present(
            draft: .init(text: text, selection: nil, isEditing: false),
            place: .init(conversation: .direct(room), inChannel: false, account: me),
            occupants: [], extensions: [], directory: nil, navigation: nil,
            rooms: [Channel(roomJID: room, name: roomName)]
        )
    }
}

@MainActor
struct RoomLinkCorrectionTests {
    @Test func aCorrectionRehydratesAndRetainsThePickedRoomLink() async throws {
        let uri = "xmpp:ops@rooms.waddle.test?join"
        let room = bare("ops@rooms.waddle.test")
        let port = FakePort()
        let session = SessionCoordinator(account: me, port: port)
        session.status.connection = .online
        session.isSendReady = true
        await session.send(Draft(
            text: "**#ops** now", references: .init(roomLinks: [RoomLinkDraft(room: room, range: 2..<6)])
        ), in: bobConversation)
        let item = try #require(session.timelines.timeline(for: bobConversation).items.last { $0.isMine })
        #expect(item.message.references == [Reference(kind: .data, uri: uri, begin: 0, end: 4)])

        let editable = EditableMessage(item: item)
        #expect(editable.references.roomLinks == [RoomLinkDraft(room: room, range: 2..<6)])
        let text = "hi " + editable.text + " #ops"
        let moved = RoomLinks.relocate(editable.references.roomLinks, from: editable.text, to: text, replacing: nil)
        #expect(await session.edit(item, draft: Draft(text: text, references: .init(roomLinks: moved))))
        #expect(port.corrections.last?.body == "hi #ops now #ops")
        #expect(port.corrections.last?.options.references == [Reference(kind: .data, uri: uri, begin: 3, end: 7)])
    }
}
