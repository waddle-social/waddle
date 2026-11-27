/// Explicit permission stays with the session. A fetched message can be
/// rebuilt with a new local row ID when its thread opens again.
struct RemoteMediaConsent {
    private struct Key: Hashable {
        enum Identity: Hashable {
            case row(String)
            case archive(String)
        }

        let conversation: ConversationID
        let identity: Identity
        let urls: MediaURLDigest
    }

    private var permissions: Set<Key> = []

    mutating func grant(_ item: TimelineItem) {
        permissions.formUnion(keys(for: item, urls: MediaURLDigest(item)))
    }

    func contains(_ item: TimelineItem, urls: MediaURLDigest) -> Bool {
        keys(for: item, urls: urls).contains(where: permissions.contains)
    }

    private func keys(for item: TimelineItem, urls: MediaURLDigest) -> [Key] {
        let row = Key(conversation: item.conversation, identity: .row(item.presentationID), urls: urls)
        // Only the archive envelope supplies this ID. Live stanza IDs
        // are insufficient evidence that a row has prior consent.
        guard case let .archive(id) = item.message.source, !id.isEmpty else { return [row] }
        return [row, Key(conversation: item.conversation, identity: .archive(id), urls: urls)]
    }
}
