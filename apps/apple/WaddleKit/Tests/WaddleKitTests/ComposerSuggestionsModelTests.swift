import Testing
@testable import WaddleKit

struct ComposerSuggestionsModelTests {
    private let room = ConversationID.room(BareJID(parsing: "general@rooms.waddle.test")!)
    private let peer = ConversationID.direct(BareJID(parsing: "friend@waddle.test")!)
    private let account = AccountIdentity(jid: BareJID(parsing: "me@waddle.test")!, nick: "me")
    private var roomPlace: ComposerSuggestionsModel.Place {
        .init(conversation: room, inChannel: true, account: account)
    }

    private let poll = ExtensionCommand(
        serviceJID: "ext.waddle.test", node: "poll", name: "Poll", scope: .channel,
        composerPrefix: "poll", inlineField: nil, composerExecute: false
    )

    @Test(arguments: ["/shrug", "/gif", "/me", "/shrug hi", "/poll"])
    func returnPassesForResolvedCommandsAndTabStillCompletes(text: String) {
        let selection = text == "/shrug hi" ? 3..<3 : nil
        let presentation = present(.init(text: text, selection: selection, isEditing: false))
        #expect(presentation.list.selected != nil)
        #expect(presentation.handle(.returnKey, in: text) == .pass)
        guard case let .pick(pick) = presentation.handle(.tab, in: text) else {
            Issue.record("Tab should complete the selected slash row")
            return
        }
        let expected = switch text {
        case "/gif": "/giphy "
        case "/shrug hi": text
        default: text + " "
        }
        #expect(pick.edit.text == expected)
        #expect(pick.clearsError)
    }

    @Test func returnPicksPartialCommandsAndKeysPassThroughAnEmptyList() {
        let partial = present("/shr")
        guard case let .pick(pick) = partial.handle(.returnKey, in: "/shr") else {
            Issue.record("Return should complete the selected partial command")
            return
        }
        #expect(pick.edit == ComposerTextEdit(text: "/shrug ", selection: 7..<7))
        let keys: [ComposerHardwareKey] = [.returnKey, .tab, .escape, .up, .down]
        for key in keys {
            #expect(present("hi").handle(key, in: "hi") == .pass)
        }
    }

    @Test func onlyEmojiSuggestionsAreOfferedInEditsAndMentionsNeedARoom() {
        let directPlace = ComposerSuggestionsModel.Place(conversation: peer, inChannel: false, account: account)
        let emoji = present(.init(text: ":joy", selection: nil, isEditing: true), place: directPlace)
        #expect(emoji.list.items.first == .emoji(.init(name: "joy", glyph: "😀")))
        #expect(emoji.directoryQuery == nil)
        for text in ["@pri", "/sh", "#gen"] {
            let editing = present(.init(text: text, selection: nil, isEditing: true))
            #expect(editing.list.items.isEmpty)
            #expect(editing.directoryQuery == nil)
        }
        let direct = present("@pri", place: directPlace)
        #expect(direct.list.items.isEmpty)
        #expect(direct.directoryQuery == nil)
        let groupDM = ComposerSuggestionsModel.Place(conversation: room, inChannel: false, account: account)
        #expect(present("/po", place: groupDM).list.items.isEmpty)
        #expect(present("/po").list.items == [.slash(.extension(poll))])
    }

    @Test func returnAcceptsTheCommandArrowedToAndRunsAnExactOneWhateverIsHighlighted() throws {
        let text = "/"
        let open = present(text)
        #expect(open.list.items.count > 1)
        guard case let .navigate(navigation) = open.handle(.down, in: text) else {
            Issue.record("Down should navigate the open list")
            return
        }
        let moved = present(text, navigation: navigation)
        #expect(moved.list.selectedIndex == 1)
        let second = try #require(moved.list.selected)
        let trigger = try #require(moved.list.trigger)
        guard case let .pick(pick) = moved.handle(.returnKey, in: text) else {
            Issue.record("Return should accept the highlighted command")
            return
        }
        #expect(pick.edit == second.accept(in: text, replacing: trigger.replacementRange))
        #expect(pick.edit != open.list.selected?.accept(in: text, replacing: trigger.replacementRange))

        // A command typed in full runs on Return, as on the web, even after the arrows have moved.
        let exact = "/shrug"
        guard case let .navigate(elsewhere) = present(exact).handle(.down, in: exact) else {
            Issue.record("Down should navigate the open list")
            return
        }
        #expect(present(exact, navigation: elsewhere).handle(.returnKey, in: exact) == .pass)
    }

    @Test func sendPicksTheHighlightedRoomMentionEmojiOrPartialCommand() throws {
        let priya = BareJID(parsing: "priya@waddle.test")!
        let cases = [
            ("#gen", "#General "), ("@pri", "@Priya "), (":joy", "😀 "), ("/shr", "/shrug "),
        ]
        for (text, completed) in cases {
            let model = present(text, occupants: [occupant("Priya", jid: priya)])
            let pick = try #require(model.pickForSubmit(in: text))
            #expect(pick.edit.text == completed)
            #expect(model.handle(.returnKey, in: text) == .pick(pick))
        }
        for text in ["/shrug", "/missing", "#missing", "@missing", ":missing", "hello"] {
            #expect(present(text).pickForSubmit(in: text) == nil)
        }

        let open = present("/")
        guard case let .navigate(navigation) = open.handle(.down, in: "/") else {
            Issue.record("Down should highlight another command")
            return
        }
        let moved = present("/", navigation: navigation)
        #expect(moved.pickForSubmit(in: "/") != open.pickForSubmit(in: "/"))
        #expect(moved.pickForSubmit(in: "/") != nil)
    }

    @Test func roomRowsDisambiguateOnlyDuplicateLabelsAmongTheResults() throws {
        let alpha = BareJID(parsing: "alpha@rooms.waddle.test")!
        let beta = BareJID(parsing: "beta@rooms.waddle.test")!
        let channels = [Channel(roomJID: alpha, name: "Ops"), Channel(roomJID: beta, name: "Ops")]
        let duplicates = present("#op", rooms: channels).list.items.compactMap(roomCandidate)
        #expect(duplicates.map(\.label) == ["#Ops", "#Ops"])
        #expect(duplicates.map(\.addressDetail) == [alpha.description, beta.description])
        #expect(duplicates.map(\.accessibilityName) == ["#Ops, \(alpha)", "#Ops, \(beta)"])

        let unique = try #require(present("#alp", rooms: channels).list.items.compactMap(roomCandidate).first)
        #expect(unique.room == alpha)
        #expect(unique.addressDetail == nil)
        #expect(unique.accessibilityName == "#Ops")
    }

    @Test func aRoomStaysSelectedWhenADuplicateLabelAppears() throws {
        let text = "#op"
        let alpha = BareJID(parsing: "alpha@rooms.waddle.test")!
        let beta = BareJID(parsing: "beta@rooms.waddle.test")!
        let other = BareJID(parsing: "other@rooms.waddle.test")!
        let channels = [Channel(roomJID: other, name: "DevOps"), Channel(roomJID: beta, name: "Ops")]
        guard case let .navigate(navigation) = present(text, rooms: channels).handle(.down, in: text) else {
            Issue.record("Down should highlight the second room")
            return
        }
        let before = present(text, navigation: navigation, rooms: channels)
        let updated = present(text, navigation: navigation, rooms: [Channel(roomJID: alpha, name: "Ops")] + channels)
        let selected = try #require(updated.list.selected.flatMap(roomCandidate))
        #expect(selected.room == beta)
        #expect(selected.addressDetail == beta.description)

        let item = try #require(before.list.selected)
        let trigger = try #require(before.list.trigger)
        #expect(updated.pick(item, from: trigger, in: text)?.reference == .roomLink(RoomLinkDraft(room: beta, range: 0..<4)))
    }

    @Test func existingRoomLinksAndUnmatchedRoomQueriesFallThrough() {
        let existing = RoomLinkDraft(room: room.jid, range: 0..<4)
        #expect(present(.init(text: "#gen", selection: nil, isEditing: false, roomLinks: [existing])).list.items.isEmpty)
        #expect(present("/ #unmatched").list.selected?.kind == .slash)
    }

    @Test func suggestionDataIsReadOnlyForTheTriggerThatNeedsIt() {
        var occupantReads = 0
        var extensionReads = 0
        var roomReads = 0
        func presentCounting(_ text: String, isEditing: Bool = false) {
            _ = ComposerSuggestionsModel.present(
                draft: .init(text: text, selection: nil, isEditing: isEditing), place: roomPlace,
                occupants: { occupantReads += 1; return [] }(),
                extensions: { extensionReads += 1; return [poll] }(), directory: nil, navigation: nil,
                rooms: { roomReads += 1; return [Channel(roomJID: room.jid, name: "General")] }()
            )
        }
        presentCounting("hello")
        presentCounting(":jo")
        presentCounting("#gen", isEditing: true)
        #expect((occupantReads, extensionReads, roomReads) == (0, 0, 0))
        presentCounting("@pri")
        #expect((occupantReads, extensionReads, roomReads) == (1, 0, 0))
        presentCounting("/shr")
        #expect((occupantReads, extensionReads, roomReads) == (1, 1, 0))
        presentCounting("#gen")
        #expect((occupantReads, extensionReads, roomReads) == (1, 1, 1))
    }

    @Test func lateDirectoryAndOccupantRowsPreserveTheMentionChosenByDown() throws {
        let text = "@pri"
        let occupants = [occupant("Pris", jid: BareJID(parsing: "pris@waddle.test")!), occupant("Priya", jid: BareJID(parsing: "priya@waddle.test")!)]
        let initial = present(text, occupants: occupants)
        let query = try #require(initial.directoryQuery)
        guard case let .navigate(navigation) = initial.handle(.down, in: text) else {
            Issue.record("Down should navigate the open list")
            return
        }
        #expect(present(text, occupants: occupants, navigation: navigation).list.selectedIndex == 1)

        // The directory has already matched this user's other fields.
        let users = [UserSearchResult(jid: BareJID(parsing: "offline@waddle.test")!, username: "Offline", displayName: "Priyanka")]
        let directory = ComposerSuggestionsModel.DirectoryResults(query: query, users: users)
        let arrival = present(text, occupants: occupants, directory: directory, navigation: navigation)
        #expect(arrival.list.items.count == 3)
        #expect(arrival.list.selectedIndex == 1)

        let joined = present(text, occupants: occupants + [occupant("Pria", jid: BareJID(parsing: "pria@waddle.test")!)],
                             directory: directory, navigation: arrival.list.navigation)
        #expect(joined.list.selectedIndex == 2)
        guard case let .pick(pick) = joined.handle(.returnKey, in: text) else {
            Issue.record("Return should take the selected mention")
            return
        }
        #expect(pick.edit == ComposerTextEdit(text: "@Priya ", selection: 7..<7))
        #expect(pick.reference == .mention(RecordedMention(token: "@Priya", target: .user(BareJID(parsing: "priya@waddle.test")!))))
        #expect(!pick.clearsError)
    }

    @Test(arguments: ["Smith", "Ali"])
    func directoryHitsUseRoomNicksWithoutDuplicatingMatchingOccupants(queryText: String) throws {
        let text = "@" + queryText
        let alice = try #require(BareJID(parsing: "alice@waddle.test"))
        let offline = try #require(BareJID(parsing: "offline@waddle.test"))
        let query = try #require(present(text).directoryQuery)
        let directory = ComposerSuggestionsModel.DirectoryResults(query: query, users: [
            .init(jid: alice, username: "alice-account", displayName: "Alice Smith"),
            .init(jid: offline, username: "Offline", displayName: "Alistair Smith"),
        ])

        let presentation = present(text, occupants: [occupant("Alice", jid: alice)], directory: directory)
        #expect(presentation.list.items == [
            .mention(.init(name: "Alice", target: .user(alice))),
            .mention(.init(name: "Offline", target: .user(offline))),
        ])
        guard case let .pick(pick) = presentation.handle(.returnKey, in: text) else {
            Issue.record("Return should take the selected mention")
            return
        }
        #expect(pick.edit == ComposerTextEdit(text: "@Alice ", selection: 7..<7))
        #expect(pick.reference == .mention(RecordedMention(token: "@Alice", target: .user(alice))))
    }

    @Test func directoryResultsBelongToOneQueryAndRoomAndEscapeKeepsItClosed() throws {
        let initial = present("@pri")
        let query = try #require(initial.directoryQuery)
        let directory = ComposerSuggestionsModel.DirectoryResults(
            query: query, users: [.init(jid: BareJID(parsing: "offline@waddle.test")!, username: "Offline", displayName: "Priya")]
        )
        #expect(present("@pr", directory: directory).list.items.isEmpty)
        let otherRoom = ComposerSuggestionsModel.Place(
            conversation: .room(BareJID(parsing: "other@rooms.waddle.test")!), inChannel: true, account: account
        )
        #expect(present("@pri", place: otherRoom, directory: directory).list.items.isEmpty)
        #expect(present("@", directory: directory).directoryQuery == nil)
        let open = present("@pri", directory: directory)
        guard case let .navigate(navigation) = open.handle(.escape, in: "@pri") else {
            Issue.record("Escape should dismiss the open list before cancelling composer context")
            return
        }
        let dismissed = present("@pri", occupants: [occupant("Priya", jid: BareJID(parsing: "priya@waddle.test")!)],
                                directory: directory, navigation: navigation)
        #expect(dismissed.list.items.isEmpty)
        #expect(dismissed.directoryQuery == nil)
        #expect(dismissed.handle(.escape, in: "@pri") == .pass)
        #expect(present("@pr", occupants: [occupant("Priya", jid: BareJID(parsing: "priya@waddle.test")!)],
                        navigation: navigation).list.items.count == 1)
    }

    @Test func stalePointerPicksRequireTheSameTriggerAndAVisibleRow() throws {
        let original = present("/sh")
        let trigger = try #require(original.list.trigger)
        let item = try #require(original.list.selected)
        #expect(original.pick(item, from: trigger, in: "/sh")?.edit.text == "/shrug ")
        #expect(present("/shr").pick(item, from: trigger, in: "/shr") == nil)

        let occupants = [occupant("Priya", jid: BareJID(parsing: "priya@waddle.test")!)]
        let mention = present("@pri", occupants: occupants)
        let mentionTrigger = try #require(mention.list.trigger)
        let mentionItem = try #require(mention.list.selected)
        #expect(present("@pri").pick(mentionItem, from: mentionTrigger, in: "@pri") == nil)
    }

    @Test func commandButtonPlacesCaretAfterTheSlashOrExistingCommandWord() {
        #expect(SlashCompletion.start(in: "") == ComposerTextEdit(text: "/", selection: 1..<1))
        #expect(SlashCompletion.start(in: "🇳🇴 hi") == ComposerTextEdit(text: "/ 🇳🇴 hi", selection: 1..<1))
        #expect(SlashCompletion.start(in: "/shrug 🇳🇴") == ComposerTextEdit(text: "/shrug 🇳🇴", selection: 6..<6))
    }

    private func present(
        _ text: String, place: ComposerSuggestionsModel.Place? = nil, occupants: [Occupant] = [],
        directory: ComposerSuggestionsModel.DirectoryResults? = nil,
        navigation: ComposerSuggestionSelection.Navigation? = nil, rooms: [Channel]? = nil
    ) -> ComposerSuggestionsModel {
        present(.init(text: text, selection: nil, isEditing: false), place: place, occupants: occupants,
                directory: directory, navigation: navigation, rooms: rooms)
    }

    private func present(
        _ draft: ComposerSuggestionsModel.Draft, place: ComposerSuggestionsModel.Place? = nil,
        occupants: [Occupant] = [], directory: ComposerSuggestionsModel.DirectoryResults? = nil,
        navigation: ComposerSuggestionSelection.Navigation? = nil, rooms: [Channel]? = nil
    ) -> ComposerSuggestionsModel {
        ComposerSuggestionsModel.present(
            draft: draft, place: place ?? roomPlace, occupants: occupants, extensions: [poll], directory: directory,
            navigation: navigation, rooms: rooms ?? [Channel(roomJID: room.jid, name: "General")]
        )
    }

    private func roomCandidate(_ item: ComposerSuggestion) -> RoomLinks.Candidate? {
        guard case let .room(candidate) = item else { return nil }
        return candidate
    }

    private func occupant(_ nick: String, jid: BareJID) -> Occupant {
        Occupant(nick: nick, availability: .available, status: nil, affiliation: .member, role: .participant,
                 realJID: jid, hats: [])
    }
}
