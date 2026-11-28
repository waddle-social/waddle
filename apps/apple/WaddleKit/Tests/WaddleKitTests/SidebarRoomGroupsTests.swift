import Foundation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct SidebarRoomGroupsTests {
    @Test func includesEmptySpacesAndKeepsEveryRoomInOneGroup() {
        let session = session()
        let groups = SidebarRoomGroups(session: session).groups

        #expect(groups.map(\.title) == ["Studio", "Planning", "Standalone rooms"])
        #expect(groups.map { $0.rooms.map(\.channel.name) } == [["general", "design"], [], ["lounge", "retreat", "workshop"]])
    }

    @Test func identifiesTheGroupToRevealForADirectoryRoom() {
        let session = session()
        #expect(SidebarRoomGroups.groupID(for: room("general").conversation, in: session.directory) == .space("studio"))
        #expect(SidebarRoomGroups.groupID(for: room("retreat").conversation, in: session.directory) == .standalone)
        #expect(SidebarRoomGroups.groupID(for: room("workshop").conversation, in: session.directory) == .standalone)
        #expect(SidebarRoomGroups.groupID(for: room("left").conversation, in: session.directory) == nil)
    }

    @Test func rollsUpRoomCountsAndHuddleParticipantsWithoutCountingThreadRepliesTwice() throws {
        let session = session()
        let general = room("general", space: "studio")
        let design = room("design", space: "studio")
        let lounge = room("lounge")
        session.unread.liveMessage(in: general.conversation, isMine: false, mentionsMe: true)
        session.unread.set(5, for: general.conversation)
        session.unread.set(3, for: design.conversation)
        session.unread.set(9, for: lounge.conversation)
        session.applyInbox(
            InboxEntry(
                partner: general.roomJID, kind: .room, lastStanzaID: "reply", lastUpdated: 100, unread: 2, preview: nil,
                thread: .init(id: "planning", title: "Launch planning")
            ))
        for (channel, nick) in [(general, "Jo"), (general, "Ada"), (design, "Mika"), (lounge, "Priya")] {
            session.calls.groupPresence.apply(
                WirePresence(
                    from: try #require(channel.roomJID.with(resource: nick)), kind: .available,
                    groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: false))
                ))
        }

        let studio = try #require(SidebarRoomGroups(session: session).groups.first)
        #expect(studio.activity.unread == 8)
        #expect(studio.activity.mentionedRoomCount == 1)
        #expect(studio.activity.huddleParticipantCount == 3)
        #expect(studio.activity.badge == .mentions(1))
        #expect(studio.rooms.map(\.huddleParticipantCount) == [2, 1])

        session.unread.clear(general.conversation)
        #expect(SidebarRoomGroups(session: session).groups.first?.activity.badge == .unread(5))
        session.unread.clear(design.conversation)
        #expect(SidebarRoomGroups(session: session).groups.first?.activity.badge == .unread(2))
        session.unread.clearThread(ThreadKey(room: general.roomJID, threadID: "planning"))
        #expect(SidebarRoomGroups(session: session).groups.first?.activity.badge == nil)
    }

    @Test func showsInboxThreadTitlesAndDropsThreadsImmediatelyWhenRead() throws {
        let session = session()
        let general = room("general", space: "studio")
        for (id, title, preview, time) in [
            ("older", nil, "Earlier reply", 100),
            ("newer", "Release checklist", "Latest reply", 200),
        ] as [(String, String?, String, Int64)] {
            session.applyInbox(
                InboxEntry(
                    partner: general.roomJID, kind: .room, lastStanzaID: id, lastUpdated: time, unread: 2,
                    preview: preview, thread: .init(id: id, title: title)
                ))
        }
        let first = try #require(SidebarRoomGroups(session: session).groups.first?.rooms.first)
        #expect(first.threads.map(\.title) == ["Release checklist", "Earlier reply"])
        #expect(first.threads.map(\.unread) == [2, 2])
        session.unread.setActiveThread(first.threads[0].key)
        #expect(SidebarRoomGroups(session: session).groups.first?.rooms.first?.threads.map(\.title) == ["Earlier reply"])
    }

    private func session() -> SessionCoordinator {
        let session = SessionCoordinator(account: AccountIdentity(jid: jid("ana@waddle.test"), nick: "Ana"), port: FakePort())
        session.directory.apply(
            Topology(
                spaces: [
                    Space(id: "studio", serviceJID: nil, name: "Studio", summary: nil),
                    Space(id: "planning", serviceJID: nil, name: "Planning", summary: nil),
                    Space(id: "standalone", serviceJID: jid("rooms.waddle.test"), name: "Rooms", summary: nil),
                ],
                channels: [
                    room("design", space: "studio", position: 2),
                    room("general", space: "studio"),
                    room("lounge"),
                    room("retreat", space: "standalone"),
                    room("workshop", space: "missing"),
                    Channel(roomJID: jid("private@rooms.waddle.test"), name: "private", isGroupDM: true),
                    Channel(roomJID: jid("left@rooms.waddle.test"), name: "left", hasLeft: true),
                ]
            ))
        return session
    }

    private func room(_ name: String, space: String? = nil, position: Int = 0) -> Channel {
        Channel(roomJID: jid("\(name)@rooms.waddle.test"), name: name, position: position, spaceID: space)
    }

    private func jid(_ value: String) -> BareJID { BareJID(parsing: value)! }
}
