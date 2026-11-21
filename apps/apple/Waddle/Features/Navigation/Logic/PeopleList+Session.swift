import WaddleKit

extension PeopleList {
    /// The people `session` knows now, named as its lists name them.
    @MainActor
    init(session: SessionCoordinator, query: String = "") {
        self = PeopleList.build(
            contacts: session.contacts ?? [],
            presence: session.presence.contacts,
            directPeers: session.directory.directConversations.map(\.peer),
            own: session.account.jid,
            name: { session.directory.title(for: .direct($0)) },
            query: query
        )
    }
}
