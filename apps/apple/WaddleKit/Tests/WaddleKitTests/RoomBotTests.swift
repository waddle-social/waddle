import Foundation
import Testing
@testable import WaddleKit

private let helper = bare("helper@extensions.waddle.test")
private let scribe = bare("scribe@extensions.waddle.test")
private let carol = bare("carol@waddle.test")

/// A bot's presence as the room sends it: the Bot hat and its real JID.
private func botPresence(_ nick: String, jid botJID: BareJID?, kind: WirePresence.Kind = .available) -> WirePresence {
    WirePresence(
        from: room.with(resource: nick)!,
        kind: kind,
        hats: [Hat(uri: Hat.botURI, title: "Bot")],
        occupant: .init(
            affiliation: RoomAffiliation.none,
            role: .participant,
            realJID: botJID.map { jid("\($0)/bot") },
            statusCodes: []
        )
    )
}

private func person(_ nick: String, role: RoomRole = .participant, hats: [Hat] = [], realJID: BareJID? = nil) -> Occupant {
    Occupant(nick: nick, availability: .available, status: nil, affiliation: .member, role: role, realJID: realJID, hats: hats)
}

@MainActor
@Suite("Declared room bots")
struct RoomBotTests {
    private func coordinator(_ port: FakePort = FakePort()) -> SessionCoordinator {
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return coordinator
    }

    // MARK: Store

    @Test func storeKeepsBotsPerRoomSortedByName() {
        let store = RoomBotStore()
        let other = bare("random@muc.waddle.test")
        let ticket = store.beginRefresh(in: room)
        store.replace(
            [RoomBot(jid: scribe, name: "scribe"), RoomBot(jid: helper, name: "Archivist"), RoomBot(jid: bare("zed@extensions.waddle.test"), name: nil)],
            in: room,
            ticket: ticket
        )
        #expect(store.bots(in: room).map(\.displayName) == ["Archivist", "scribe", "zed"])
        #expect(store.bots(in: other).isEmpty)
        #expect(store.isDeclared(helper, in: room))
        #expect(!store.isDeclared(helper, in: other))
        #expect(!store.isDeclared(carol, in: room))
    }

    @Test func botWithoutAUsableNameShowsItsLocalpart() {
        #expect(RoomBot(jid: helper, name: nil).displayName == "helper")
        #expect(RoomBot(jid: helper, name: "  ").displayName == "helper")
        #expect(RoomBot(jid: helper, name: " Helper Bot ").displayName == "Helper Bot")
    }

    @Test func storeDropsAnAnswerSupersededByANewerRefresh() {
        let store = RoomBotStore()
        let older = store.beginRefresh(in: room)
        let newer = store.beginRefresh(in: room)
        store.replace([RoomBot(jid: helper, name: "helper")], in: room, ticket: newer)
        store.replace([], in: room, ticket: older)
        #expect(store.isDeclared(helper, in: room))
    }

    @Test func clearForgetsBotsAndInFlightRefreshes() {
        let store = RoomBotStore()
        let ticket = store.beginRefresh(in: room)
        store.clear()
        store.replace([RoomBot(jid: helper, name: "helper")], in: room, ticket: ticket)
        #expect(store.bots(in: room).isEmpty)
    }

    // MARK: Loading

    @Test func refreshStoresWhatThePortListsAndKeepsThemOnFailure() async {
        let port = FakePort()
        port.roomBots[room] = [RoomBot(jid: helper, name: "helper")]
        let coordinator = coordinator(port)
        await coordinator.refreshRoomBots(in: room)
        #expect(coordinator.roomBots.isDeclared(helper, in: room))

        port.failsRoomBots = true
        await coordinator.refreshRoomBots(in: room)
        #expect(coordinator.roomBots.isDeclared(helper, in: room))

        port.failsRoomBots = false
        port.roomBots[room] = []
        await coordinator.refreshRoomBots(in: room)
        #expect(coordinator.roomBots.bots(in: room).isEmpty)
    }

    @Test func openingARoomLoadsItsBots() async {
        let port = FakePort()
        port.roomBots[room] = [RoomBot(jid: helper, name: "helper")]
        let coordinator = coordinator(port)
        await coordinator.open(roomConversation)
        #expect(port.roomBotRequests == [room])
        #expect(coordinator.roomBots.isDeclared(helper, in: room))
    }

    @Test func openingADirectConversationDoesNotAskForBots() async {
        let port = FakePort()
        let coordinator = coordinator(port)
        await coordinator.open(bobConversation)
        #expect(port.roomBotRequests.isEmpty)
    }

    @Test func signOutForgetsDeclaredBots() async {
        let port = FakePort()
        port.roomBots[room] = [RoomBot(jid: helper, name: "helper")]
        let coordinator = coordinator(port)
        await coordinator.refreshRoomBots(in: room)
        await coordinator.stop()
        #expect(coordinator.roomBots.bots(in: room).isEmpty)
    }

    // MARK: Refetch on a bot-hatted presence

    @Test func botHattedPresenceForAnUnlistedJIDRefetches() async {
        let port = FakePort()
        let coordinator = coordinator(port)
        coordinator.handle(.presence(botPresence("helper", jid: helper)))
        await eventually { !port.roomBotRequests.isEmpty }
        #expect(port.roomBotRequests == [room])
    }

    @Test func botsLeavePresenceAfterTheServerRecordedItListsIt() async {
        let port = FakePort()
        let coordinator = coordinator(port)
        // The listing records the room only once the bot's send settled, so
        // the join finds nothing and the leave finds the bot.
        coordinator.handle(.presence(botPresence("helper", jid: helper)))
        await eventually { port.roomBotRequests.count == 1 }
        port.roomBots[room] = [RoomBot(jid: helper, name: "helper")]
        coordinator.handle(.presence(botPresence("helper", jid: helper, kind: .unavailable)))
        await eventually { coordinator.roomBots.isDeclared(helper, in: room) }
        #expect(coordinator.roomBots.isDeclared(helper, in: room))
    }

    @Test func botHattedPresenceForAListedJIDDoesNotRefetch() async {
        let port = FakePort()
        port.roomBots[room] = [RoomBot(jid: helper, name: "helper")]
        let coordinator = coordinator(port)
        await coordinator.refreshRoomBots(in: room)
        port.roomBotRequests.removeAll()
        coordinator.handle(.presence(botPresence("helper", jid: helper)))
        coordinator.handle(.presence(botPresence("helper", jid: helper, kind: .unavailable)))
        try? await Task.sleep(nanoseconds: 50_000_000)
        #expect(port.roomBotRequests.isEmpty)
    }

    @Test func presenceWithoutTheBotHatOrARealJIDDoesNotRefetch() async {
        let port = FakePort()
        let coordinator = coordinator(port)
        let hatless = WirePresence(
            from: room.with(resource: "carol")!,
            kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: jid("carol@waddle.test/phone"), statusCodes: [])
        )
        coordinator.handle(.presence(hatless))
        coordinator.handle(.presence(botPresence("helper", jid: nil)))
        try? await Task.sleep(nanoseconds: 50_000_000)
        #expect(port.roomBotRequests.isEmpty)
    }

    // MARK: Roster

    @Test func hattedMomentaryOccupantIsNotListedAsAPerson() {
        let bot = person("helper", hats: [Hat(uri: Hat.botURI, title: "Bot")])
        let vip = person("dana", hats: [Hat(uri: "urn:waddle:hats:vip", title: "VIP")])
        let groups = MemberRoster.grouped([bot, vip, person("alice", role: .moderator)])
        #expect(groups.map(\.role) == [.moderator, .participant])
        #expect(groups.flatMap(\.occupants).map(\.nick) == ["alice", "dana"])
    }

    @Test func rosterGroupsByRoleAndSortsByNick() {
        let groups = MemberRoster.grouped([
            person("zoe"), person("Bea"), person("vic", role: .visitor), person("mod", role: .moderator),
        ])
        #expect(groups.map(\.role) == [.moderator, .participant, .visitor])
        #expect(groups[1].occupants.map(\.nick) == ["Bea", "zoe"])
    }

    @Test func absentMembersAreAffiliatedPeopleNotInTheRoom() {
        let members = [
            RoomMember(jid: bob, nick: "bob", affiliation: .member),
            RoomMember(jid: carol, nick: nil, affiliation: .owner),
            RoomMember(jid: bare("dave@waddle.test"), nick: "Reserved", affiliation: .member),
            RoomMember(jid: bare("erin@waddle.test"), nick: nil, affiliation: .outcast),
            RoomMember(jid: bare("fay@waddle.test"), nick: nil, affiliation: .admin),
        ]
        // Bob is here by real JID, Dave by his reserved nick; outcasts are not members.
        let present = [person("bobby", realJID: bob), person("reserved")]
        let absent = MemberRoster.absent(members, present: present)
        #expect(absent.map(\.jid) == [carol, bare("fay@waddle.test")])
    }

    @Test func affiliationChangesUpdateALoadedList() {
        let members = [RoomMember(jid: bob, nick: "bob", affiliation: .member)]
        let promoted = MemberRoster.updating(members, jid: bob, to: .admin)
        #expect(promoted == [RoomMember(jid: bob, nick: "bob", affiliation: .admin)])
        #expect(MemberRoster.updating(promoted, jid: bob, to: .none).isEmpty)
        #expect(MemberRoster.updating(members, jid: carol, to: .member).map(\.jid) == [bob, carol])
    }
}
