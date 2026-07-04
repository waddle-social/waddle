import Foundation

// MARK: - Forum Threads

extension AppModel {
    func sendForumTopic(title: String, body: String) async {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        await rustClient.sendForumTopic(roomJID: roomJID, body: body, title: title)
    }

    func sendForumReply(body: String, threadID: String) async {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        await rustClient.sendForumReply(roomJID: roomJID, body: body, threadID: threadID)
    }

    var forumTopics: [ChatTimelineMessage] {
        chatStore.messages.filter { $0.isForumTopic }
    }

    func threadReplies(for threadID: String) -> [ChatTimelineMessage] {
        chatStore.messages.filter { $0.threadID == threadID && $0.isForumReply }
    }
}
