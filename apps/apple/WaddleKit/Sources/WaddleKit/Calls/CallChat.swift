/// The chat of a room's call (web `CallChatPanel`): the thread its
/// announcement heads. While the call goes on its stage offers it, and
/// afterwards it is an ordinary thread, reached from the announcement.
public enum CallChat {
    /// The thread that is the chat of the call going on in a room now: the
    /// thread of the room's latest announcement, unless the room has said
    /// that call is over. `nil` when no announcement of it is loaded, as
    /// for a call that began before we came into the room.
    public static func thread(in items: [TimelineItem]) -> String? {
        guard let id = CallAnchorCard.newestAnnouncementID(in: items),
              let announcement = items.last(where: { $0.id == id }),
              announcement.callEnded == nil else { return nil }
        return announcement.message.thread
    }

    /// How many of `replies`, oldest first, are other people's messages
    /// that came after the one last seen. With nothing seen yet, or with
    /// that message no longer among them, all of them are new.
    public static func unread(in replies: [TimelineItem], after lastSeen: String?) -> Int {
        let start = lastSeen.flatMap { id in replies.lastIndex { $0.id == id } }.map { $0 + 1 } ?? 0
        return replies[start...].filter(\.countsAsUnread).count
    }

    /// The stage's button: "Call chat", "Call chat, 3 unread".
    public static func buttonTitle(unread: Int, isOpen: Bool) -> String {
        if isOpen { return "Hide call chat" }
        return unread > 0 ? "Call chat, \(unread) unread" : "Call chat"
    }

    /// The link on a call's card: "3 messages in call chat", "Call chat".
    public static func linkTitle(messages: Int) -> String {
        switch messages {
        case 0: "Call chat"
        case 1: "1 message in call chat"
        default: "\(messages) messages in call chat"
        }
    }

    /// The count on the button's badge: no more than two digits wide.
    public static func badge(_ unread: Int) -> String? {
        guard unread > 0 else { return nil }
        return unread > 99 ? "99+" : "\(unread)"
    }
}
