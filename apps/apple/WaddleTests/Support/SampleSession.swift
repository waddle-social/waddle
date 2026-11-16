import SwiftUI
@testable import Waddle
@testable import WaddleKit

/// A signed-in session with a small, fixed community: the people and rooms
/// the styleguide's frames use. Nothing touches the network.
@MainActor
final class SampleSession {
    static let ana = AccountIdentity(jid: bare("ana@waddle.test"), nick: "Ana Lindqvist")

    static let general = bare("general@rooms.waddle.test")
    static let releaseTrain = bare("release-train@rooms.waddle.test")
    static let designCritique = bare("design-critique@rooms.waddle.test")
    static let design = bare("design@rooms.waddle.test")

    static let priya = bare("priya@waddle.test")
    static let jo = bare("jo@waddle.test")
    static let mika = bare("mika@waddle.test")
    static let theo = bare("theo@waddle.test")

    let port = FakePort()
    let coordinator: SessionCoordinator
    let navigation = NavigationModel()
    let app = AppState()

    init() {
        coordinator = SessionCoordinator(account: Self.ana, port: port)
        coordinator.status.connection = .online

        coordinator.directory.apply(Topology(
            spaces: [Space(id: "hq", serviceJID: nil, name: "Waddle HQ", summary: nil)],
            channels: [
                Channel(roomJID: Self.general, name: "general", position: 0, spaceID: "hq"),
                Channel(roomJID: Self.releaseTrain, name: "release-train", position: 1, spaceID: "hq"),
                Channel(roomJID: Self.designCritique, name: "design-critique", kind: .forum, position: 2, spaceID: "hq"),
                Channel(roomJID: Self.design, name: "design", position: 3, spaceID: "hq"),
            ]
        ))

        // release-train: three unread, one of them a mention. design: nine unread.
        for index in 0..<3 {
            coordinator.unread.liveMessage(in: .room(Self.releaseTrain), isMine: false, mentionsMe: index == 0)
        }
        coordinator.unread.set(9, for: .room(Self.design))

        let people: [(BareJID, String?, TimeInterval, String)] = [
            (Self.priya, nil, 600, "Shipping the digest build now."),
            (Self.jo, "away", 500, "Back in ten, grabbing coffee."),
            (Self.mika, nil, 400, "Tagging the RC after the huddle."),
            (Self.theo, "dnd", 300, "Reviewing the identity drafts."),
        ]
        for (jid, show, age, preview) in people {
            coordinator.directory.touchDirect(jid, at: date(-age), preview: preview)
            _ = coordinator.presence.apply(WirePresence(
                from: jid.with(resource: "phone")!,
                kind: .available,
                show: show,
                status: nil,
                hats: [],
                occupant: nil,
                idleSince: nil
            ))
        }
    }

    /// Fills the Inbox with the unread messages behind the room counts, as
    /// a refresh would after fetching them, so the screen never fetches.
    func seedInbox() {
        let messages: [BareJID: [(nick: String, body: String, age: TimeInterval)]] = [
            Self.releaseTrain: [
                ("Jo", "@ana can you check room history on Android before we tag?", 300),
                ("Mika", "Server tests are green on main. Tagging the RC after the huddle.", 200),
            ],
            Self.design: [
                ("Sam", "New identity drafts are up. The waddling typing dots are my favourite part.", 900),
            ],
        ]
        let store = coordinator.unreadOverview
        let refresh = store.begin()
        var sections: Set<UnreadOverviewSection> = []
        for candidate in coordinator.overviewCandidates() {
            let section = UnreadOverviewSection(
                room: candidate.room,
                threadID: nil,
                unread: candidate.unread,
                lastStanzaID: candidate.lastStanzaID
            )
            let items = (messages[candidate.room] ?? []).enumerated().map { index, entry in
                let wire = WireMessage(
                    source: .live,
                    type: .groupchat,
                    from: candidate.room.with(resource: entry.nick)!,
                    to: Self.ana.jid.with(resource: "phone")!,
                    identity: MessageIdentity(messageID: nil, originID: nil, stanzaID: nil, stanzaIDs: []),
                    timestamp: date(-entry.age),
                    body: entry.body
                )
                return TimelineItem(
                    id: "\(candidate.room)-\(index)",
                    conversation: .room(candidate.room),
                    isMine: false,
                    message: wire,
                    body: entry.body
                )
            }
            store.remember(items, for: section, refresh: refresh)
            sections.insert(section)
        }
        store.finish(refresh: refresh, keeping: sections, didFail: false)
    }

    /// `view` with everything a signed-in screen reads from the environment.
    func host<V: View>(_ view: V) -> some View {
        view
            .environment(app)
            .environment(coordinator)
            .environment(navigation)
    }
}
