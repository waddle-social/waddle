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

    static let rink = Story(
        id: "story-rink",
        body: "First ice of the season at the rink.",
        media: URL(string: "https://upload.waddle.test/story-rink.jpg")!,
        mediaType: "image/jpeg",
        author: priya,
        postedAt: date(-1200),
        expiresAt: date(80_000)
    )

    /// Fills the Feed as a refresh would: three posts, three stories (one
    /// seen), and reactions on the first story.
    func seedCommunity() {
        let store = coordinator.community
        let refresh = store.begin()
        store.publish(posts: [
            FeedPost(
                id: "post-3",
                body: "Release candidate is tagged. Thanks to everyone who tested room history on Android this week.",
                author: Self.mika,
                publishedAt: date(-420),
                link: URL(string: "https://waddle.social/releases")
            ),
            FeedPost(id: "post-2", body: "Jo is feeling happy", author: Self.jo, publishedAt: date(-3600), source: .mood),
            FeedPost(
                id: "post-1",
                body: "New identity drafts are up in design-critique. The waddling typing dots are my favourite part.",
                author: Self.theo,
                publishedAt: date(-90_000)
            ),
        ], refresh: refresh)
        store.publish(stories: [
            Self.rink,
            Story(
                id: "story-desk", media: URL(string: "https://upload.waddle.test/story-desk.jpg")!,
                mediaType: "image/jpeg", author: Self.jo, postedAt: date(-5400), expiresAt: date(70_000)
            ),
            Story(
                id: "story-huddle", media: URL(string: "https://upload.waddle.test/story-huddle.jpg")!,
                mediaType: "image/jpeg", author: Self.theo, postedAt: date(-9000), expiresAt: date(60_000)
            ),
        ], refresh: refresh)
        store.mergeReads(["story-huddle": date(-600)])
        store.set(
            StoryReactions(
                counts: [StoryReactions.Count(emoji: "❤️", count: 3), StoryReactions.Count(emoji: "🎉", count: 1)],
                mine: ["❤️"]
            ),
            of: Self.rink.id,
            askedAt: 0
        )
        store.finish(feedFailed: false, storiesFailed: false)
    }

    /// `view` with everything a signed-in screen reads from the environment.
    func host<V: View>(_ view: V) -> some View {
        view
            .environment(app)
            .environment(coordinator)
            .environment(navigation)
    }
}
