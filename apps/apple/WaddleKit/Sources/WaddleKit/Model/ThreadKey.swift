import Foundation

/// A XEP-0201 thread inside a room: the unit the server inbox keeps a
/// separate thread row for, and the Waddle MAM thread filter queries.
public struct ThreadKey: Hashable, Sendable {
    public let room: BareJID
    /// The thread id, which for a thread started from a message is that
    /// message's id.
    public let threadID: String

    public init(room: BareJID, threadID: String) {
        self.room = room
        self.threadID = threadID
    }
}
