import Foundation
import os

private let logger = Logger(subsystem: "social.waddle.ios", category: "RustXmppClient")

struct XMPPTopologyDiscoveryResult: Sendable, Equatable {
    let topology: XMPPDiscoveredTopology
    let errorDescription: String?

    var failed: Bool {
        errorDescription != nil
    }
}

final class DiscoveryErrorTracker: @unchecked Sendable {
    private let lock = NSLock()
    private var latestTopologyError: String?

    func clear() {
        lock.lock()
        defer { lock.unlock() }
        latestTopologyError = nil
    }

    func record(_ description: String) -> Bool {
        guard description.hasPrefix("discover_topology failed:") else { return false }
        lock.lock()
        defer { lock.unlock() }
        latestTopologyError = description
        return true
    }

    func take() -> String? {
        lock.lock()
        defer { lock.unlock() }
        let value = latestTopologyError
        latestTopologyError = nil
        return value
    }
}

// MARK: - RustXmppClient

/// Thin adapter that bridges `WaddleClient` (generated UniFFI binding) to a Swift-friendly API.
/// Pulls one ordered UniFFI event at a time so native durability barriers
/// complete before Rust advances the stream.
@MainActor
final class RustXmppClient: ObservableObject {
    @Published fileprivate(set) var connectionState: XMPPConnectionState = .disconnected

    private let waddleClient: WaddleClient
    private let discoveryErrors: DiscoveryErrorTracker
    private let eventPump: NativeXMPPEventPump

    init(config: WaddleConfig) {
        let client = WaddleClient(config: config)
        let discoveryErrors = DiscoveryErrorTracker()
        self.waddleClient = client
        self.discoveryErrors = discoveryErrors
        self.eventPump = NativeXMPPEventPump(
            client: client,
            mapper: NativeXMPPEventMapper(
                expectedAttempt: config.deliveryAttempt,
                discoveryErrors: discoveryErrors
            )
        )
    }

    // MARK: - Connection

    func connect() async -> Bool {
        connectionState = .connecting
        let connected = await eventPump.connect()
        if !connected {
            connectionState = .disconnected
        }
        return connected
    }

    func nextEvent() async -> NativeXMPPPullResult {
        let result = await eventPump.nextEvent()
        switch result {
        case .event(.sessionReady(_, _)):
            connectionState = .ready
        case .event(.disconnected), .closed:
            connectionState = .disconnected
        case .event, .consumed:
            if !eventPump.isConnected {
                connectionState = .disconnected
            }
        }
        return result
    }

    func disconnect() async {
        connectionState = .disconnecting
        await eventPump.disconnect()
        connectionState = .disconnected
    }

    // MARK: - Messaging

    @discardableResult
    func sendChatMessage(peerJID: String, body: String, options: WaddleSendOptions? = nil) async -> WaddleSendMessageOutcome {
        await waddleClient.sendChatMessage(peerJid: peerJID, body: body, options: options)
    }

    @discardableResult
    func sendGroupchatMessage(
        roomJID: String,
        body: String,
        options: WaddleSendOptions? = nil
    ) async -> WaddleSendMessageOutcome {
        await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: options)
    }

    // Forum topics/replies are groupchat messages with a thread.
    func sendForumTopic(roomJID: String, body: String, title: String? = nil) async {
        let options = title.map { topicTitle in
            WaddleSendOptions(
                stanzaId: nil,
                subject: topicTitle,
                reply: nil,
                fallback: nil,
                thread: nil,
                markupSpans: [],
                references: [],
                sharedFiles: [],
                linkPreviewToken: nil,
                requestDisplayedMarker: false,
                mucPm: false
            )
        }
        _ = await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: options)
    }

    func sendForumReply(roomJID: String, body: String, threadID: String) async {
        let options = WaddleSendOptions(
            stanzaId: nil,
            subject: nil,
            reply: nil,
            fallback: nil,
            thread: WaddleThreadTarget(id: threadID, parent: nil),
            markupSpans: [],
            references: [],
            sharedFiles: [],
            linkPreviewToken: nil,
            requestDisplayedMarker: false,
            mucPm: false
        )
        _ = await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: options)
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

    func sendPresence(status: String? = nil, show: String? = nil, idleSince: String? = nil) async {
        await waddleClient.sendPresence(status: status, show: show, idleSince: idleSince)
    }

    // MARK: - Avatar (XEP-0084)

    /// Fetch the published PEP avatar for a JID. Returns nil when the user
    /// hasn't published one or the fetch fails (errors surface via the event
    /// ordered event stream's error path; this call never throws).
    func requestAvatar(jid: String) async -> WaddleAvatar? {
        await waddleClient.requestAvatar(jid: jid)
    }

    // MARK: - Direct messages

    @discardableResult
    func sendDirectMessage(peerJID: String, body: String, options: WaddleSendOptions? = nil) async -> WaddleSendMessageOutcome {
        await waddleClient.sendChatMessage(peerJid: peerJID, body: body, options: options)
    }

    // MARK: - Groupchat extras

    func sendGroupchatMessageWithMarkup(roomJID: String, body: String, spans: [XMPPMarkupSpan]) async {
        // Markup spans are not yet re-wired through the FFI send path.
        _ = await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: body, options: nil)
    }

    func retractMessage(roomJID: String, messageID: String) async {}

    // MARK: - Spaces topology

    func discoverTopology() async -> XMPPTopologyDiscoveryResult {
        discoveryErrors.clear()
        let topology = await waddleClient.discoverTopology()
        return XMPPTopologyDiscoveryResult(
            topology: XMPPDiscoveredTopology(
                spaces: topology.spaces.map {
                    XMPPDiscoveredSpace(
                        id: $0.id,
                        serviceJID: $0.serviceJid,
                        name: $0.name,
                        description: $0.description
                    )
                },
                channels: topology.channels.map {
                    XMPPDiscoveredChannel(
                        id: $0.id,
                        roomJID: $0.roomJid,
                        name: $0.name,
                        description: $0.description,
                        channelType: $0.channelType,
                        position: Int($0.position),
                        spaceID: $0.spaceId
                    )
                }
            ),
            errorDescription: discoveryErrors.take()
        )
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
            disposition: inferredDisposition(for: mediaType),
            encrypted: nil
        )
        let options = WaddleSendOptions(
            stanzaId: nil,
            subject: nil,
            reply: nil,
            fallback: nil,
            thread: nil,
            markupSpans: [],
            references: [],
            sharedFiles: [sharedFile],
            linkPreviewToken: nil,
            requestDisplayedMarker: false,
            mucPm: false
        )
        _ = await waddleClient.sendGroupchatMessage(roomJid: roomJID, body: fileURL, options: options)
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

// MARK: - Ordered event mapping

final class NativeXMPPEventMapper {
    private let discoveryErrors: DiscoveryErrorTracker
    private let expectedAttempt: WaddleDeliveryAttemptRef

    init(
        expectedAttempt: WaddleDeliveryAttemptRef,
        discoveryErrors: DiscoveryErrorTracker = DiscoveryErrorTracker()
    ) {
        self.discoveryErrors = discoveryErrors
        self.expectedAttempt = expectedAttempt
    }

    // Exhaustive by design: a new `WaddleClientEvent` variant upstream is a
    // compile error here until the app decides how to surface it.
    func map(_ event: WaddleClientEvent) -> NativeXMPPEventMapping {
        switch event {
        case let .sessionReady(kind, attempt):
            guard sameAttempt(attempt, expectedAttempt) else {
                return mismatchedAttempt("session-ready", attempt)
            }
            logger.info("RustXmppClient: connected")
            return .event(
                .sessionReady(
                    kind: makeSessionReadyKind(kind),
                    attempt: makeDeliveryAttempt(attempt)
                )
            )
        case .disconnected:
            return .disconnected
        case let .message(message):
            return .event(.message(makeMessage(message)))
        case let .presence(presence):
            return .event(.presence(makePresence(presence)))
        case let .mamResult(message):
            // MAM results are returned by the explicit history calls.
            logger.debug("RustXmppClient: MAM result id=\(message.mamId)")
            return .consumed
        case let .deliveryAcked(signal):
            guard sameAttempt(signal.attempt, expectedAttempt) else {
                return mismatchedAttempt("delivery-acked", signal.attempt)
            }
            return .event(
                .messageDeliveryAcked(makeDeliverySignal(signal))
            )
        case let .deliveryFailed(signal):
            guard sameAttempt(signal.attempt, expectedAttempt) else {
                return mismatchedAttempt("delivery-failed", signal.attempt)
            }
            return .event(
                .messageDeliveryFailed(makeDeliverySignal(signal))
            )
        case let .call(callEvent):
            return .event(.call(makeCallEvent(callEvent)))
        case let .authenticationFailed(condition):
            let mapped = makeSaslCondition(condition)
            if mapped.retryDisposition != .retry {
                return .selfFence(.authenticationFailed(mapped))
            }
            return .event(.authenticationFailed(mapped))
        case let .resumeFailed(transition, affected):
            // Apple currently never supplies a resume snapshot. Receiving
            // this transition is therefore a contradictory native state;
            // self-fence without issuing another native pull.
            return .selfFence(
                .error(
                    "Unexpected failed-resume transition \(transition.old.attemptId.value) affecting \(affected.count) stanzas"
                )
            )
        case let .resumeStateChanged(attempt, state):
            guard sameAttempt(attempt, expectedAttempt) else {
                return mismatchedAttempt("resume-state", attempt)
            }
            // The Apple client does not persist XEP-0198 resume state yet;
            // it reconnects with a fresh stream (`WaddleConfig.resumeState`
            // stays nil in `AppModel+Xmpp.swift`).
            logger.debug("RustXmppClient: resume snapshot \(state == nil ? "cleared" : "updated")")
            return .consumed
        case let .error(description):
            print("[RustXmppClient] Rust FFI onError: \(description)")
            if discoveryErrors.record(description) {
                return .consumed
            }
            return .event(.error(description))
        }
    }

    private func mismatchedAttempt(
        _ event: String,
        _ received: WaddleDeliveryAttemptRef
    ) -> NativeXMPPEventMapping {
        .selfFence(
            .error(
                "Native \(event) attempt mismatch: expected \(expectedAttempt.attemptId.value)/\(expectedAttempt.connectionGeneration.value), received \(received.attemptId.value)/\(received.connectionGeneration.value)"
            )
        )
    }

    private func makeMessage(_ message: WaddleMessage) -> XMPPMessageEvent {
        let timestamp = message.timestamp.flatMap { parseRFC3339($0) }
        return XMPPMessageEvent(
            from: message.from,
            to: message.to,
            type: message.messageType,
            id: message.id,
            stanzaID: message.stanzaId,
            body: message.body,
            subject: message.subject,
            thread: message.thread,
            timestamp: timestamp,
            replacesID: message.replacesId,
            retractsID: message.retractsId,
            reactionTargetID: message.reactionTargetId,
            reactionEmojis: message.reactionEmojis,
            replyToID: message.replyToId,
            replyToSender: message.replyToSender,
            replyFallbackRange: makeFallbackRange(message.replyFallbackStart, message.replyFallbackEnd),
            markupSpans: message.markupSpans.compactMap(makeMarkupSpan),
            chatState: message.chatState.map(chatStateWireName),
            displayedMarkerID: message.displayedMarkerId,
            sharedFiles: message.sharedFiles.map(makeSharedFile),
            broadcastMention: message.broadcastMention,
            mentionURIs: message.mentionUris,
            forumPostKind: message.forumPostKind.map(forumPostKindWireName),
            forumTitle: message.forumTitle,
            threadID: message.thread,
            parentThreadID: message.parentThreadId,
            isSticker: message.isSticker
        )
    }

    private func makePresence(_ presence: WaddlePresence) -> XMPPPresenceEvent {
        XMPPPresenceEvent(
            from: presence.from,
            to: presence.to,
            type: presence.presenceType,
            status: presence.status,
            show: presence.show,
            hats: presence.hats.map { XMPPPresenceHat(uri: $0.uri, title: $0.title) },
            mucAffiliation: presence.mucAffiliation.map(makeMucAffiliation),
            mucRole: presence.mucRole.map(makeMucRole)
        )
    }
}

// MARK: - Conversion helpers

func sameAttempt(
    _ lhs: WaddleDeliveryAttemptRef,
    _ rhs: WaddleDeliveryAttemptRef
) -> Bool {
    lhs.attemptId.value == rhs.attemptId.value
        && lhs.connectionGeneration.value == rhs.connectionGeneration.value
}

func makeDeliveryAttempt(
    _ attempt: WaddleDeliveryAttemptRef
) -> XMPPDeliveryAttemptRef {
    XMPPDeliveryAttemptRef(
        id: attempt.attemptId.value,
        connectionGeneration: attempt.connectionGeneration.value
    )
}

func makeDeliverySignal(
    _ signal: WaddleNativeDeliverySignal
) -> XMPPDeliverySignal {
    XMPPDeliverySignal(
        attempt: makeDeliveryAttempt(signal.attempt),
        stanzaID: signal.stanzaId.value
    )
}

func makeSessionReadyKind(
    _ kind: WaddleSessionReadyKind
) -> XMPPSessionReadyKind {
    switch kind {
    case .fresh: return .fresh
    case .resumed: return .resumed
    }
}

func makeSaslCondition(_ condition: WaddleSaslCondition) -> XMPPSaslCondition {
    switch condition {
    case .aborted: return .aborted
    case .accountDisabled: return .accountDisabled
    case .credentialsExpired: return .credentialsExpired
    case .encryptionRequired: return .encryptionRequired
    case .incorrectEncoding: return .incorrectEncoding
    case .invalidAuthzid: return .invalidAuthzid
    case .invalidMechanism: return .invalidMechanism
    case .malformedRequest: return .malformedRequest
    case .mechanismTooWeak: return .mechanismTooWeak
    case .notAuthorized: return .notAuthorized
    case .temporaryAuthFailure: return .temporaryAuthFailure
    case .unknown: return .unknown
    }
}

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

private func makeEncryptedSource(
    _ encrypted: WaddleEncryptedFile,
    fallbackURL: String
) -> XMPPEncryptedSource {
    XMPPEncryptedSource(
        url: encrypted.sources.first ?? fallbackURL,
        keyBase64: encrypted.keyB64,
        ivBase64: encrypted.ivB64,
        cipher: encrypted.cipher
    )
}

private func makeCallMedia(_ media: WaddleCallMedia) -> XMPPCallMedia {
    XMPPCallMedia(audio: media.audio, video: media.video)
}

private func makeLiveKitJoin(_ join: WaddleLiveKitJoin) -> XMPPLiveKitJoin {
    XMPPLiveKitJoin(url: join.url, room: join.room, identity: join.identity, token: join.token)
}

private func makeJingleReason(_ reason: WaddleJingleReason) -> XMPPJingleReason {
    // Exhaustive match: adding a XEP-0166 condition upstream is a
    // compile error here, by design — silently mapping a new wire
    // value to a placeholder would leave the call UI in a wrong
    // post-hangup state.
    switch reason {
    case .alternativeSession: return .alternativeSession
    case .busy: return .busy
    case .cancel: return .cancel
    case .connectivityError: return .connectivityError
    case .decline: return .decline
    case .expired: return .expired
    case .failedApplication: return .failedApplication
    case .failedTransport: return .failedTransport
    case .generalError: return .generalError
    case .gone: return .gone
    case .incompatibleParameters: return .incompatibleParameters
    case .mediaError: return .mediaError
    case .securityError: return .securityError
    case .success: return .success
    case .timeout: return .timeout
    case .unsupportedApplications: return .unsupportedApplications
    case .unsupportedTransports: return .unsupportedTransports
    }
}

/// Translate the UniFFI-generated call event into the Swift-native
/// `XMPPCallEvent`. Strict pattern match — every variant of
/// `WaddleCallEventKind` must be handled here, so adding a new
/// variant in the Rust crate is a compile error in Swift until
/// the UI catches up. That's deliberate: ringing/finish/terminate
/// is the kind of state machine where silently dropping a variant
/// would surface as a stuck UI bug.
private func makeCallEvent(_ event: WaddleCallEvent) -> XMPPCallEvent {
    let kind: XMPPCallEventKind = {
        switch event.kind {
        case let .propose(media):
            return .propose(media: makeCallMedia(media))
        case .ringing:
            return .ringing
        case .proceed:
            return .proceed
        case let .reject(reason, tieBreak):
            return .reject(reason: reason.map(makeJingleReason), tieBreak: tieBreak)
        case let .retract(reason, tieBreak):
            return .retract(reason: reason.map(makeJingleReason), tieBreak: tieBreak)
        case let .finish(reason, migratedTo):
            return .finish(reason: reason.map(makeJingleReason), migratedTo: migratedTo)
        case let .sessionInitiate(join, media):
            return .sessionInitiate(join: makeLiveKitJoin(join), media: makeCallMedia(media))
        case let .sessionAccept(join, media):
            return .sessionAccept(join: makeLiveKitJoin(join), media: makeCallMedia(media))
        case let .sessionTerminate(reason):
            return .sessionTerminate(reason: reason.map(makeJingleReason))
        }
    }()
    return XMPPCallEvent(from: event.from, to: event.to, sid: event.sid, kind: kind)
}

private func makeMarkupSpan(_ span: WaddleMarkupSpan) -> XMPPMarkupSpan? {
    let type: XMPPMarkupSpan.SpanType = {
        switch span.spanType {
        case .bold: return .bold
        case .italic: return .italic
        case .strikethrough: return .strikethrough
        case .code: return .code
        case .codeBlock: return .codeBlock
        case .blockquote: return .blockquote
        case .link: return .link
        }
    }()
    return XMPPMarkupSpan(type: type, start: Int(span.start), end: Int(span.end), uri: span.uri)
}

private func chatStateWireName(_ state: WaddleChatState) -> String {
    switch state {
    case .active: return "active"
    case .composing: return "composing"
    case .paused: return "paused"
    case .inactive: return "inactive"
    case .gone: return "gone"
    }
}

private func forumPostKindWireName(_ kind: WaddleForumPostKind) -> String {
    switch kind {
    case .topic: return "topic"
    case .reply: return "reply"
    }
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
        encryptedSource: file.encrypted.map { makeEncryptedSource($0, fallbackURL: file.url) }
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
            let eventID = archived.messageType == "groupchat"
                ? archived.stanzaId
                : (archived.originId ?? archived.id ?? archived.stanzaId)
            let msgEvent = XMPPMessageEvent(
                from: archived.from,
                to: archived.to,
                type: archived.messageType,
                id: eventID,
                stanzaID: archived.stanzaId,
                body: archived.body,
                subject: archived.subject,
                thread: archived.thread,
                timestamp: timestamp,
                replacesID: archived.replacesId,
                retractsID: archived.retractsId,
                reactionTargetID: archived.reactionTargetId,
                reactionEmojis: archived.reactionEmojis,
                replyToID: archived.replyToId,
                replyToSender: archived.replyToSender,
                replyFallbackRange: makeFallbackRange(archived.replyFallbackStart, archived.replyFallbackEnd),
                markupSpans: archived.markupSpans.compactMap(makeMarkupSpan),
                chatState: nil,
                displayedMarkerID: nil,
                sharedFiles: archived.sharedFiles.map(makeSharedFile),
                broadcastMention: archived.broadcastMention,
                mentionURIs: archived.mentionUris,
                forumPostKind: archived.forumPostKind.map(forumPostKindWireName),
                forumTitle: archived.forumTitle,
                threadID: archived.thread,
                parentThreadID: archived.parentThreadId,
                isSticker: archived.isSticker
            )
            return XMPPArchiveMessage(
                mamID: archived.mamId,
                queryID: archived.queryId,
                stanzaID: archived.stanzaId,
                delayedDeliveryTimestamp: timestamp,
                message: msgEvent,
                callEvent: archived.callEvent.map(makeCallEvent)
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
