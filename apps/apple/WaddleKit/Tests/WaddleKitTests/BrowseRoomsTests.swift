import Foundation
import Observation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct BrowseRoomsTests {
    @Test func roomsAreSortedByHuddleSizeThenMentionsUnreadRecencyAndName() {
        let lounge = room("Lounge")
        let design = room("Design")
        let mention = room("Mention")
        let unread = room("Unread")
        let latest = room("Latest")
        let session = session(with: [lounge, design, mention, unread, latest, room("Beta"), room("alpha")])
        advertiseCall(in: lounge, nick: "Jo", video: true, session: session)
        advertiseCall(in: lounge, nick: "Mika", session: session)
        advertiseCall(in: design, nick: "Ada", session: session)
        session.unread.liveMessage(in: mention.conversation, isMine: false, mentionsMe: true)
        session.unread.set(12, for: unread.conversation)
        setPreview("  The updated notes are ready.  ", at: 200, in: latest, session: session)

        let page = BrowseRooms(session: session)
        #expect(page.rows.map(\.channel.name) == ["Lounge", "Design", "Mention", "Unread", "Latest", "alpha", "Beta"])
        #expect(page.countText == "7 rooms · 2 live")
        #expect(page.rows.first?.metadata == "Video huddle · 2 in")
        #expect(page.rows.first?.action == .open)
        #expect(page.rows.first(where: { $0.id == mention.roomJID })?.mentionsMe == true)
        #expect(page.rows.first(where: { $0.id == unread.roomJID })?.unread == 12)
        #expect(page.rows.first(where: { $0.id == latest.roomJID })?.preview == "The updated notes are ready.")
    }

    @Test func onlyRoomsAreListedAndTheAvailableActionReflectsMembership() {
        let joined = room("Joined", autojoin: false)
        let discoverable = room("Discoverable", autojoin: false)
        let left = room("Left", autojoin: false, hasLeft: true)
        let regular = room("Regular")
        let groupDM = room("Private", isGroupDM: true)
        let session = session(with: [joined, discoverable, left, regular, groupDM])
        markJoined(joined, session: session)
        session.directory.touchDirect(BareJID(parsing: "ada@waddle.test")!, at: .now, preview: "Hi!")

        let rows = BrowseRooms(session: session).rows
        #expect(rows.map(\.channel.name) == ["Discoverable", "Joined", "Left", "Regular"])
        #expect(rows.map(\.action) == [.joinRoom, .open, .joinRoom, .open])
        #expect(rows.first(where: { $0.id == joined.roomJID })?.metadata == "Room · 1 here")
    }

    @Test func aLiveHuddleInAnOccupiedRoomOffersToJoinTheCall() {
        let general = room("General", autojoin: false)
        let session = session(with: [general])
        session.status.connection = .online
        markJoined(general, session: session)
        advertiseCall(in: general, nick: "Jo", video: true, session: session)

        let row = BrowseRooms(session: session).rows.first
        #expect(row?.action == .joinCall)
        #expect(row?.action.title == "Join")
        #expect(row?.action.accessibilityLabel(roomName: general.name) == "Join the huddle in General")
    }

    @Test func aLiveHuddleOffersOpenWhenWeHaveNotEnteredItsRoomOrAreOffline() {
        let general = room("General", autojoin: false)
        let session = session(with: [general])
        session.status.connection = .online
        advertiseCall(in: general, nick: "Jo", session: session)
        #expect(BrowseRooms(session: session).rows.first?.action == .open)

        markJoined(general, session: session)
        session.status.connection = .signedOut
        #expect(BrowseRooms(session: session).rows.first?.action == .open)
    }

    @Test func aLiveHuddleOffersOpenWhileTheCallSlotIsTaken() async {
        let general = room("General")
        let session = session(with: [general])
        session.status.connection = .online
        markJoined(general, session: session)
        advertiseCall(in: general, nick: "Jo", session: session)
        let peer = BareJID(parsing: "jo@waddle.test")!
        #expect(await session.calls.startCall(to: peer, media: CallMedia(audio: true, video: false)))

        #expect(BrowseRooms(session: session).rows.first?.action == .open)
        session.calls.clear()
    }

    @Test func newestThreadCanSupplyThePreviewWithoutAddingItsUnreadAgain() {
        let general = room("General")
        let design = room("Design")
        let session = session(with: [general, design])
        setPreview("Latest room message", at: 100, in: general, session: session)
        setPreview("Newer room message", at: 200, in: design, session: session)
        session.applyInbox(
            InboxEntry(
                partner: general.roomJID, kind: .room, lastStanzaID: "reply", lastUpdated: 300, unread: 3,
                preview: nil, thread: .init(id: "discussion", title: "Release planning")
            ))

        let rows = BrowseRooms(session: session).rows
        #expect(rows.map(\.id) == [general.roomJID, design.roomJID])
        #expect(rows.first?.preview == "Release planning")
        #expect(rows.first?.unread == 0)
    }

    @Test func aPreviewChangeIsObservedWithoutAnUnreadChange() {
        let general = room("General")
        let session = session(with: [general])
        setPreview("Before", at: 100, in: general, session: session)
        let observation = PreviewObservation()
        let before = withObservationTracking {
            BrowseRooms(session: session)
        } onChange: {
            MainActor.assumeIsolated { observation.didChange = true }
        }
        #expect(before.rows.first?.preview == "Before")

        setPreview("After", at: 200, in: general, session: session)

        #expect(observation.didChange)
        #expect(BrowseRooms(session: session).rows.first?.preview == "After")
        #expect(session.unread.count(for: general.conversation) == 0)
    }

    private func session(with rooms: [Channel]) -> SessionCoordinator {
        let account = AccountIdentity(jid: BareJID(parsing: "ana@waddle.test")!, nick: "Ana")
        let session = SessionCoordinator(account: account, port: FakePort())
        session.directory.apply(Topology(spaces: [], channels: rooms))
        return session
    }

    private func room(_ name: String, autojoin: Bool = true, hasLeft: Bool = false, isGroupDM: Bool = false) -> Channel {
        Channel(
            roomJID: BareJID(parsing: "\(name.lowercased())@rooms.waddle.test")!, name: name,
            autojoin: autojoin, isGroupDM: isGroupDM, hasLeft: hasLeft)
    }

    private func setPreview(_ preview: String, at time: Int64, in channel: Channel, session: SessionCoordinator) {
        _ = session.inbox.apply(
            InboxEntry(
                partner: channel.roomJID, kind: .room, lastStanzaID: "message-\(time)",
                lastUpdated: time, unread: 0, preview: preview
            ))
    }

    private func markJoined(_ channel: Channel, session: SessionCoordinator) {
        _ = session.presence.apply(
            WirePresence(
                from: channel.roomJID.with(resource: "Ana")!, kind: .available,
                occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110])
            ))
    }

    private func advertiseCall(in channel: Channel, nick: String, video: Bool = false, session: SessionCoordinator) {
        session.calls.groupPresence.apply(
            WirePresence(
                from: channel.roomJID.with(resource: nick)!, kind: .available,
                groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: video))
            ))
    }

    @MainActor
    private final class PreviewObservation {
        var didChange = false
    }
}
