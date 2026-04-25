import Foundation
import os

private let logger = Logger(subsystem: "social.waddle.ios", category: "RustXmppClient")

// MARK: - RustXmppClient

/// Thin adapter that bridges `WaddleClient` (generated UniFFI binding) to a Swift-friendly API.
/// Implements `WaddleEventListener` so the Rust layer can push events back to Swift.
@MainActor
final class RustXmppClient: ObservableObject {
    @Published fileprivate(set) var connectionState: XMPPConnectionState = .disconnected

    // Public event stream — consumers iterate with `for await event in rustClient.events`.
    let events: AsyncStream<XMPPEvent>

    private let continuation: AsyncStream<XMPPEvent>.Continuation
    private let waddleClient: WaddleClient
    private let eventListener: _EventListener

    init(config: WaddleConfig) {
        var continuation: AsyncStream<XMPPEvent>.Continuation!
        self.events = AsyncStream { continuation = $0 }
        self.continuation = continuation

        let listener = _EventListener(continuation: continuation)
        self.eventListener = listener
        self.waddleClient = WaddleClient(config: config, listener: listener)
        listener.owner = self
    }

    deinit {
        continuation.finish()
    }

    // MARK: - Connection

    func connect() async {
        connectionState = .connecting
        await waddleClient.connect()
    }

    func disconnect() async {
        connectionState = .disconnecting
        await waddleClient.disconnect()
        connectionState = .disconnected
    }

    // MARK: - Messaging

    func sendChatMessage(peerJID: String, body: String, options: WaddleSendOptions? = nil) async {
        await waddleClient.sendChatMessage(peerJid: peerJID, body: body, options: options)
    }

    func sendGroupchatMessage(
        roomJID: String,
        body: String,
        options: WaddleSendOptions? = nil
    ) async {
        await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: options)
    }

    // Forum topics/replies are groupchat messages with a thread.
    func sendForumTopic(roomJID: String, body: String, title: String? = nil) async {
        await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: nil)
    }

    func sendForumReply(roomJID: String, body: String, threadID: String) async {
        let options = WaddleSendOptions(
            reply: nil,
            fallback: nil,
            thread: WaddleThreadTarget(id: threadID, parent: nil),
            sharedFiles: []
        )
        await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: options)
    }

    // MARK: - History

    func fetchDmHistory(peerJID: String, max: UInt32, before: String? = nil) async -> XMPPArchivePage {
        let page = await waddleClient.fetchDmHistory(peerJid: peerJID, maxMessages: max, beforeId: before)
        return page.toXMPPArchivePage()
    }

    func fetchRoomHistory(roomJID: String, max: UInt32, before: String? = nil) async -> XMPPArchivePage {
        let page = await waddleClient.fetchRoomHistory(roomJid: roomJID, maxMessages: max, beforeId: before)
        return page.toXMPPArchivePage()
    }

    // MARK: - Rooms

    func joinRoom(_ roomJID: String, nick: String) async {
        await waddleClient.joinRoom(roomJid: roomJID, nick: nick)
    }

    func leaveRoom(_ roomJID: String, nick: String) async {
        await waddleClient.leaveRoom(roomJid: roomJID, nick: nick)
    }

    // MARK: - Presence

    func sendPresence(status: String? = nil, show: String? = nil) async {
        await waddleClient.sendPresence(status: status, show: show)
    }

    // MARK: - Avatar (XEP-0084)

    /// Fetch the published PEP avatar for a JID. Returns nil when the user
    /// hasn't published one or the fetch fails (errors surface via the event
    /// listener's `on_error` path; this call never throws).
    func requestAvatar(jid: String) async -> WaddleAvatar? {
        await waddleClient.requestAvatar(jid: jid)
    }

    // MARK: - Direct messages

    func sendDirectMessage(peerJID: String, body: String, options: WaddleSendOptions? = nil) async {
        await waddleClient.sendChatMessage(peerJid: peerJID, body: body, options: options)
    }

    // MARK: - Groupchat extras

    func sendGroupchatMessageWithMarkup(roomJID: String, body: String, spans: [XMPPMarkupSpan]) async {
        // Markup spans are not yet re-wired through the FFI send path.
        await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: nil)
    }

    func retractMessage(roomJID: String, messageID: String) async {}

    // MARK: - Room list (implicit single space)

    func listRooms() async -> [WaddleRoom] {
        return await waddleClient.listRooms()
    }

    // MARK: - Chat state / markers stubs

    func sendDisplayedMarker(roomJID: String, messageID: String) async {}
    func sendChatState(roomJID: String, state: String) async {}

    // MARK: - Presence (no-arg overload)

    func sendPresence() async {
        await sendPresence(status: nil, show: nil)
    }

    // MARK: - Channel creation stub

    func createChannel(name: String, description: String?, channelType: String, position: Int) async -> CreateChannelResult? { nil }

    // MARK: - Stubs (features not yet in WaddleClient)

    func fetchInbox() async -> [XMPPInboxEntry] { [] }

    func publishMood(_ mood: String, text: String?) async {}
    func clearMood() async {}

    func publishActivity(_ activity: String, text: String?) async {}

    func publishTune(artist: String?, title: String?, source: String?, uri: String?) async {}
    func clearTune() async {}

    func enablePushNotifications(pushServiceJID: String, node: String, token: String) async {}

    func discoverUploadService() async -> String? {
        await waddleClient.discoverUploadService()
    }
    func requestUploadSlot(
        serviceJID: String, filename: String, size: Int, contentType: String
    ) async -> (putURL: String, getURL: String, putHeaders: [(String, String)])? {
        guard size >= 0 else { return nil }
        guard let slot = await waddleClient.requestUploadSlot(
            serviceJid: serviceJID,
            filename: filename,
            size: UInt64(size),
            contentType: contentType
        ) else {
            return nil
        }
        return (
            putURL: slot.putUrl,
            getURL: slot.getUrl,
            putHeaders: slot.putHeaders.map { ($0.name, $0.value) }
        )
    }

    func sendGroupchatFileMessage(
        roomJID: String, fileURL: String, fileName: String, mediaType: String, size: Int
    ) async {
        guard size >= 0 else { return }
        let sharedFile = WaddleSharedFile(
            url: fileURL,
            name: fileName,
            mediaType: mediaType,
            size: UInt64(size),
            width: nil,
            height: nil,
            disposition: inferredDisposition(for: mediaType)
        )
        let options = WaddleSendOptions(
            reply: nil,
            fallback: nil,
            thread: nil,
            sharedFiles: [sharedFile]
        )
        await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: fileURL, options: options)
    }
}

// MARK: - Errors

enum RustClientError: LocalizedError {
    case notImplemented

    var errorDescription: String? {
        switch self {
        case .notImplemented: return "This feature is not yet implemented in the Rust client."
        }
    }
}

// MARK: - Event listener (non-actor helper)

/// Non-actor implementation of `WaddleEventListener` that forwards events to the AsyncStream.
/// Must be a class (reference type) to satisfy UniFFI's AnyObject requirement.
private final class _EventListener: WaddleEventListener {
    private let continuation: AsyncStream<XMPPEvent>.Continuation
    // Set by `RustXmppClient.init` after the listener is constructed so the
    // listener can drive the owning client's `connectionState` without a
    // retain cycle. Reads on non-main threads are safe — Swift weak references
    // are atomic; the property is only mutated on the MainActor hop below.
    weak var owner: RustXmppClient?

    init(continuation: AsyncStream<XMPPEvent>.Continuation) {
        self.continuation = continuation
    }

    func onConnected() {
        let owner = self.owner
        Task { @MainActor in
            owner?.connectionState = .ready
            logger.info("RustXmppClient: connected")
        }
        continuation.yield(.sessionReady)
    }

    func onDisconnected() {
        let owner = self.owner
        Task { @MainActor in
            owner?.connectionState = .disconnected
        }
        continuation.yield(.disconnected)
    }

    func onError(description: String) {
        print("[RustXmppClient] Rust FFI onError: \(description)")
        continuation.yield(.error(description))
    }

    func onMessage(message: WaddleMessage) {
        let timestamp = message.timestamp.flatMap { parseRFC3339($0) }
        let event = XMPPMessageEvent(
            from: message.from,
            to: message.to,
            type: message.messageType,
            id: message.id,
            stanzaID: message.stanzaId,
            body: message.body,
            subject: nil,
            thread: message.thread,
            timestamp: timestamp,
            replacesID: message.replacesId,
            retractsID: message.retractsId,
            reactionTargetID: message.reactionTargetId,
            reactionEmojis: message.reactionEmojis,
            replyToID: message.replyToId,
            replyToSender: message.replyToSender,
            replyFallbackRange: makeFallbackRange(message.replyFallbackStart, message.replyFallbackEnd),
            markupSpans: [],
            chatState: nil,
            displayedMarkerID: nil,
            sharedFiles: message.sharedFiles.map(makeSharedFile),
            broadcastMention: nil,
            mentionURIs: [],
            forumPostKind: nil,
            forumTitle: nil,
            threadID: message.thread,
            parentThreadID: message.parentThreadId,
            isSticker: false
        )
        continuation.yield(.message(event))
    }

    func onPresence(presence: WaddlePresence) {
        let event = XMPPPresenceEvent(
            from: presence.from,
            to: presence.to,
            type: presence.presenceType,
            status: presence.status,
            show: presence.show,
            hats: presence.hats.map { XMPPPresenceHat(uri: $0.uri, title: $0.title) },
            mucAffiliation: presence.mucAffiliation.map(makeMucAffiliation),
            mucRole: presence.mucRole.map(makeMucRole)
        )
        continuation.yield(.presence(event))
    }

    func onMamResult(message: WaddleArchivedMessage) {
        // MAM results are delivered via the fetchDmHistory / fetchRoomHistory return values;
        // individual callbacks here are informational only.
        logger.debug("RustXmppClient: MAM result id=\(message.mamId)")
    }
}

// MARK: - Conversion helpers

/// Combine the two `Option<u32>` UniFFI fields into a `Range<Int>?`. A range is
/// only produced when both ends are present and `end > start`.
private func makeFallbackRange(_ start: UInt32?, _ end: UInt32?) -> Range<Int>? {
    guard let start, let end, end > start else { return nil }
    return Int(start)..<Int(end)
}

private func makeMucAffiliation(_ affiliation: WaddleMucAffiliation) -> XMPPMucAffiliation {
    switch affiliation {
    case .owner: return .owner
    case .admin: return .admin
    case .member: return .member
    case .outcast: return .outcast
    case .none: return .none
    }
}

private func makeMucRole(_ role: WaddleMucRole) -> XMPPMucRole {
    switch role {
    case .moderator: return .moderator
    case .participant: return .participant
    case .visitor: return .visitor
    case .none: return .none
    }
}

private func parseRFC3339(_ string: String) -> Date? {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let date = formatter.date(from: string) { return date }
    formatter.formatOptions = [.withInternetDateTime]
    return formatter.date(from: string)
}

private func makeSharedFile(_ file: WaddleSharedFile) -> XMPPSharedFile {
    XMPPSharedFile(
        url: file.url,
        name: file.name,
        mediaType: file.mediaType,
        size: file.size.flatMap(Int.init),
        width: file.width.flatMap(Int.init),
        height: file.height.flatMap(Int.init),
        disposition: file.disposition,
        encryptedSource: nil
    )
}

private func inferredDisposition(for mediaType: String) -> String {
    if mediaType.hasPrefix("image/")
        || mediaType.hasPrefix("video/")
        || mediaType.hasPrefix("audio/")
        || mediaType == "application/pdf" {
        return "inline"
    }
    return "attachment"
}

private extension WaddleMamPage {
    func toXMPPArchivePage() -> XMPPArchivePage {
        let converted = messages.map { archived -> XMPPArchiveMessage in
            let timestamp = archived.timestamp.flatMap { parseRFC3339($0) }
            let msgEvent = XMPPMessageEvent(
                from: archived.from,
                to: archived.to,
                type: archived.messageType,
                id: archived.stanzaId,
                stanzaID: archived.stanzaId,
                body: archived.body,
                subject: nil,
                thread: archived.thread,
                timestamp: timestamp,
                replacesID: nil,
                retractsID: nil,
                reactionTargetID: archived.reactionTargetId,
                reactionEmojis: archived.reactionEmojis,
                replyToID: archived.replyToId,
                replyToSender: archived.replyToSender,
                replyFallbackRange: makeFallbackRange(archived.replyFallbackStart, archived.replyFallbackEnd),
                markupSpans: [],
                chatState: nil,
                displayedMarkerID: nil,
                sharedFiles: archived.sharedFiles.map(makeSharedFile),
                broadcastMention: nil,
                mentionURIs: [],
                forumPostKind: nil,
                forumTitle: nil,
                threadID: archived.thread,
                parentThreadID: archived.parentThreadId,
                isSticker: false
            )
            return XMPPArchiveMessage(
                mamID: archived.mamId,
                queryID: archived.queryId,
                stanzaID: archived.stanzaId,
                delayedDeliveryTimestamp: timestamp,
                message: msgEvent
            )
        }

        let pageInfo = XMPPRSMPageInfo(
            first: firstId,
            last: lastId,
            count: nil,
            index: nil,
            isComplete: isComplete
        )

        return XMPPArchivePage(messages: converted, pageInfo: pageInfo)
    }
}
