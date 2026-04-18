import Foundation

enum XMPPServiceError: LocalizedError {
    case notReady
    case disconnected
    case timeout(String)
    case iqError(String)

    var errorDescription: String? {
        switch self {
        case .notReady:
            return "XMPP session is not ready yet."
        case .disconnected:
            return "The XMPP session disconnected before the request completed."
        case .timeout(let message):
            return message
        case .iqError(let message):
            return message
        }
    }
}

@MainActor
final class XMPPService: ObservableObject {
    @Published private(set) var connectionState: XMPPConnectionState = .disconnected
    @Published private(set) var boundJID: String?

    let events: AsyncStream<XMPPEvent>

    private let continuation: AsyncStream<XMPPEvent>.Continuation
    private let transport = XMPPWebSocketTransport()
    private var receiveTask: Task<Void, Never>?
    private var credentials: XMPPCredentials?
    private var bufferedXML = ""
    private var authenticated = false
    private var streamFeatures: XMPPStreamFeatures?
    private var bindRequestID = 0
    private var sessionRequestID = 0
    private var iqRequestID = 0
    private var pendingRequest: PendingRequest?
    private var pendingIQResponses: [String: CheckedContinuation<XMPPElement, Error>] = [:]
    private var pendingArchiveQuery: PendingArchiveQuery?

    private enum PendingRequest {
        case bind(id: String)
        case session(id: String)
    }

    private final class PendingArchiveQuery {
        let id: String
        let roomJID: String
        let continuation: CheckedContinuation<XMPPArchivePage, Error>
        var messages: [XMPPArchiveMessage] = []
        var timeoutTask: Task<Void, Never>?

        init(id: String, roomJID: String, continuation: CheckedContinuation<XMPPArchivePage, Error>) {
            self.id = id
            self.roomJID = roomJID
            self.continuation = continuation
        }
    }

    init() {
        var continuation: AsyncStream<XMPPEvent>.Continuation!
        events = AsyncStream { continuation = $0 }
        self.continuation = continuation
    }

    func connect(session: WaddleSession) async throws {
        await disconnect(emitEvent: false)

        let credentials = session.xmppCredentials
        self.credentials = credentials
        authenticated = false
        streamFeatures = nil
        boundJID = nil
        bufferedXML.removeAll()
        pendingRequest = nil

        connectionState = .connecting
        try await transport.connect(to: credentials.websocketURL)
        try await transport.send(XMPPXML.openStream(to: credentials.domain))
        connectionState = .negotiating

        receiveTask = Task { [weak self] in
            await self?.receiveLoop()
        }
    }

    func disconnect(emitEvent: Bool = true) async {
        connectionState = .disconnecting
        receiveTask?.cancel()
        receiveTask = nil
        await transport.close()
        bufferedXML.removeAll()
        pendingRequest = nil
        authenticated = false
        streamFeatures = nil
        resumeAllPendingIQs(with: XMPPServiceError.disconnected)
        resumePendingArchiveQuery(with: XMPPServiceError.disconnected)
        credentials = nil
        boundJID = nil
        connectionState = .disconnected
        if emitEvent {
            continuation.yield(.disconnected)
        }
    }

    func sendPresence(status: String? = nil, show: String? = nil) async throws {
        try ensureReady()
        try await transport.send(XMPPXML.presence(show: show, status: status))
    }

    func joinRoom(_ roomJID: String, nick: String? = nil) async throws {
        try ensureReady()
        let roomNick = nick ?? credentials?.username ?? "waddle"
        try await transport.send(XMPPXML.joinRoom(roomJID: roomJID, nick: roomNick))
    }

    func sendGroupchatMessage(roomJID: String, body: String, thread: String? = nil) async throws {
        try ensureReady()
        try await transport.send(XMPPXML.groupchatMessage(to: roomJID, body: body, thread: thread))
    }

    func sendGroupchatReplyMessage(
        roomJID: String,
        body: String,
        replyToID: String,
        replyToSender: String?,
        replyToBody: String?,
        thread: String? = nil
    ) async throws {
        try ensureReady()
        try await transport.send(
            XMPPXML.groupchatReplyMessage(
                to: roomJID,
                body: body,
                replyToID: replyToID,
                replyToSender: replyToSender,
                replyToBody: replyToBody,
                thread: thread
            )
        )
    }

    func fetchRoomHistory(roomJID: String, max: Int = 50, before: String? = "") async throws -> XMPPArchivePage {
        try ensureReady()
        guard pendingArchiveQuery == nil else {
            throw XMPPServiceError.notReady
        }

        let id = nextIQID(prefix: "mam")
        return try await withCheckedThrowingContinuation { continuation in
            let request = PendingArchiveQuery(id: id, roomJID: roomJID, continuation: continuation)
            pendingArchiveQuery = request
            request.timeoutTask = Task { [weak self] in
                try? await Task.sleep(nanoseconds: 15_000_000_000)
                await self?.handleArchiveQueryTimeout(id: id, roomJID: roomJID)
            }

            Task { [weak self] in
                guard let self else { return }
                do {
                    try await self.transport.send(XMPPXML.mamRoomHistoryQuery(id: id, to: roomJID, max: max, before: before))
                } catch {
                    self.resumePendingArchiveQuery(with: error)
                }
            }
        }
    }

    func discoverWaddles() async throws -> [XMPPDiscoveredWaddle] {
        try ensureReady()
        guard let credentials else {
            return []
        }

        let service = "spaces.\(jidDomain(credentials.jid))"
        let itemsID = nextIQID(prefix: "disco-items")
        let itemsElement = try await sendIQ(
            XMPPXML.discoItems(id: itemsID, to: service),
            id: itemsID
        )

        var discovered: [XMPPDiscoveredWaddle] = []
        for item in XMPPXML.parseDiscoItems(from: itemsElement) {
            let waddleID = item.node ?? item.jid.flatMap { barePeerJID($0).split(separator: "@").first.map(String.init) } ?? ""
            guard !waddleID.isEmpty else { continue }

            var name = item.name ?? waddleID
            var isPublic = true

            do {
                let infoID = nextIQID(prefix: "disco-info")
                let infoElement = try await sendIQ(
                    XMPPXML.discoInfo(id: infoID, to: service, node: waddleID),
                    id: infoID
                )
                if let title = XMPPXML.discoFieldValue(from: infoElement, named: "pubsub#title")
                    ?? XMPPXML.discoIdentityName(from: infoElement),
                   !title.isEmpty {
                    name = title
                }
                if let accessModel = XMPPXML.discoFieldValue(from: infoElement, named: "pubsub#access_model")?.lowercased() {
                    isPublic = accessModel != "whitelist"
                }
            } catch {
                // Keep the discovery item metadata when the info lookup fails.
            }

            discovered.append(XMPPDiscoveredWaddle(id: waddleID, name: name, isPublic: isPublic))
        }

        return discovered
    }

    func discoverChannels(waddleID: String) async throws -> [XMPPDiscoveredChannel] {
        try ensureReady()
        guard let credentials else {
            return []
        }

        let service = "spaces.\(jidDomain(credentials.jid))"
        let itemsID = nextIQID(prefix: "channel-items")
        let itemsElement = try await sendIQ(
            XMPPXML.discoItems(id: itemsID, to: service, node: waddleID),
            id: itemsID
        )

        let prefix = "\(waddleID)_"
        var channels: [XMPPDiscoveredChannel] = []

        for (index, item) in XMPPXML.parseDiscoItems(from: itemsElement).enumerated() {
            let channelID: String = {
                if let itemJID = item.jid,
                   let parsed = parseManagedRoomBareJID(itemJID),
                   parsed.waddleID == waddleID {
                    return parsed.channelID
                }

                if let itemJID = item.jid {
                    let localpart = barePeerJID(itemJID).split(separator: "@").first.map(String.init) ?? ""
                    if localpart.hasPrefix(prefix) {
                        return String(localpart.dropFirst(prefix.count))
                    }
                    return localpart
                }

                return item.node ?? ""
            }()

            guard !channelID.isEmpty else { continue }

            var channelType = "text"
            if let itemJID = item.jid {
                do {
                    let infoID = nextIQID(prefix: "channel-info")
                    let infoElement = try await sendIQ(
                        XMPPXML.discoInfo(id: infoID, to: itemJID),
                        id: infoID
                    )
                    let features = XMPPXML.discoFeatures(from: infoElement)
                    let forumFlag = XMPPXML.discoFieldValue(from: infoElement, named: "muc#roomconfig_forum")?.lowercased()
                    if features.contains("urn:xmpp:forums:0") || ["1", "true", "yes"].contains(forumFlag ?? "") {
                        channelType = "forum"
                    }
                } catch {
                    // Default to text when room info is unavailable.
                }
            }

            channels.append(
                XMPPDiscoveredChannel(
                    id: channelID,
                    name: item.name ?? channelID,
                    channelType: channelType,
                    position: index
                )
            )
        }

        return channels
    }

    private func receiveLoop() async {
        defer {
            let shouldEmitDisconnect: Bool
            if Task.isCancelled {
                shouldEmitDisconnect = false
            } else if case .failed = connectionState {
                shouldEmitDisconnect = false
            } else {
                shouldEmitDisconnect = true
            }

            if shouldEmitDisconnect {
                connectionState = .disconnected
                continuation.yield(.disconnected)
            }
        }

        while !Task.isCancelled {
            do {
                guard let xml = try await transport.receive() else {
                    break
                }
                handleIncomingText(xml)
            } catch {
                fail(error.localizedDescription)
                break
            }
        }
    }

    private func handleIncomingText(_ text: String) {
        bufferedXML += text
        let documents = XMPPXML.splitDocuments(from: &bufferedXML)
        for document in documents {
            guard let element = XMPPXML.parseDocument(document) else {
                continue
            }
            handleIncomingElement(element)
        }
    }

    private func handleIncomingElement(_ element: XMPPElement) {
        switch element.localName {
        case "features":
            handleStreamFeatures(element)
        case "success":
            authenticated = true
            pendingRequest = nil
            connectionState = .negotiating
            continuation.yield(.authenticated)
            if let credentials {
                Task {
                    try? await transport.send(XMPPXML.openStream(to: credentials.domain))
                }
            }
        case "failure":
            continuation.yield(.authenticationFailed(element.text.isEmpty ? nil : element.text))
            fail("XMPP authentication failed.")
        case "iq":
            handleIQ(element)
        case "message":
            if handleArchiveMessage(element) {
                return
            }
            continuation.yield(.message(XMPPXML.parseMessage(from: element)))
        case "presence":
            continuation.yield(.presence(XMPPXML.parsePresence(from: element)))
        case "error":
            if let streamError = XMPPXML.streamError(from: element) {
                continuation.yield(.streamError(name: streamError.name, text: streamError.text))
                fail(streamError.text ?? streamError.name)
            } else {
                continuation.yield(.error(element.text.isEmpty ? "Unknown XMPP error." : element.text))
            }
        default:
            break
        }
    }

    private func handleStreamFeatures(_ element: XMPPElement) {
        guard let features = XMPPXML.parseStreamFeatures(from: element) else {
            return
        }

        streamFeatures = features
        continuation.yield(.streamFeatures(features))

        if !authenticated {
            guard features.supportsOAUTHBearer, let credentials else {
                fail("Server does not advertise OAUTHBEARER.")
                return
            }
            connectionState = .authenticating
            let auth = XMPPXML.authenticationRequest(jid: credentials.bareJID, bearerToken: credentials.bearerToken)
            Task {
                try? await transport.send(auth)
            }
            return
        }

        if pendingRequest == nil {
            if features.supportsBind {
                bindResource()
            } else {
                connectionState = .ready
                continuation.yield(.sessionReady)
            }
        }
    }

    private func handleIQ(_ element: XMPPElement) {
        guard let type = element.attribute("type") else {
            return
        }

        switch type {
        case "result":
            if let pendingRequest {
                switch pendingRequest {
                case .bind(let id) where element.attribute("id") == id:
                    if let jid = XMPPXML.parseBoundJID(from: element) {
                        boundJID = jid
                        continuation.yield(.resourceBound(jid: jid))
                    }
                    self.pendingRequest = nil
                    if streamFeatures?.supportsSession == true {
                        requestSession()
                    } else {
                        connectionState = .ready
                        continuation.yield(.sessionReady)
                    }
                    return
                case .session(let id) where element.attribute("id") == id:
                    self.pendingRequest = nil
                    connectionState = .ready
                    continuation.yield(.sessionReady)
                    return
                default:
                    break
                }
            }

            if handleArchiveResult(element) {
                return
            }

            guard let id = element.attribute("id"),
                  let continuation = pendingIQResponses.removeValue(forKey: id) else {
                return
            }
            continuation.resume(returning: element)
        case "error":
            let message = element.text.isEmpty ? "XMPP IQ error." : element.text
            if let pendingArchiveQuery, element.attribute("id") == pendingArchiveQuery.id {
                resumePendingArchiveQuery(with: XMPPServiceError.iqError(message))
                return
            }
            if let id = element.attribute("id"),
               let continuation = pendingIQResponses.removeValue(forKey: id) {
                continuation.resume(throwing: XMPPServiceError.iqError(message))
            } else {
                continuation.yield(.error(message))
                fail(message)
            }
        default:
            break
        }
    }

    private func bindResource() {
        guard pendingRequest == nil, let credentials else { return }
        bindRequestID += 1
        let id = "bind-\(bindRequestID)"
        pendingRequest = .bind(id: id)
        connectionState = .binding
        Task {
            try? await transport.send(XMPPXML.bind(resource: credentials.resource, id: id))
        }
    }

    private func requestSession() {
        guard pendingRequest == nil else { return }
        sessionRequestID += 1
        let id = "session-\(sessionRequestID)"
        pendingRequest = .session(id: id)
        connectionState = .binding
        Task {
            try? await transport.send(XMPPXML.requestSession(id: id))
        }
    }

    private func fail(_ message: String) {
        connectionState = .failed(message)
        resumeAllPendingIQs(with: XMPPServiceError.iqError(message))
        resumePendingArchiveQuery(with: XMPPServiceError.iqError(message))
        continuation.yield(.error(message))
        Task {
            await transport.close()
        }
    }

    private func nextIQID(prefix: String) -> String {
        iqRequestID += 1
        return "\(prefix)-\(iqRequestID)"
    }

    private func ensureReady() throws {
        guard connectionState == .ready else {
            throw XMPPServiceError.notReady
        }
    }

    private func sendIQ(_ xml: String, id: String) async throws -> XMPPElement {
        try ensureReady()

        return try await withCheckedThrowingContinuation { continuation in
            pendingIQResponses[id] = continuation

            Task { [weak self] in
                guard let self else { return }
                do {
                    try await self.transport.send(xml)
                } catch {
                    self.resumePendingIQ(id: id, with: error)
                }
            }
        }
    }

    private func resumePendingIQ(id: String, with error: Error) {
        guard let continuation = pendingIQResponses.removeValue(forKey: id) else {
            return
        }
        continuation.resume(throwing: error)
    }

    private func handleArchiveMessage(_ element: XMPPElement) -> Bool {
        guard let pendingArchiveQuery,
              let archiveMessage = XMPPXML.parseMamResult(from: element) else {
            return false
        }

        if let queryID = archiveMessage.queryID, queryID != pendingArchiveQuery.id {
            return false
        }

        pendingArchiveQuery.messages.append(archiveMessage)
        return true
    }

    private func handleArchiveResult(_ element: XMPPElement) -> Bool {
        guard let pendingArchiveQuery,
              element.attribute("id") == pendingArchiveQuery.id else {
            return false
        }

        if let finQueryID = element.firstChild(named: "fin")?.attribute("queryid"),
           finQueryID != pendingArchiveQuery.id {
            return false
        }

        let pageInfo = XMPPXML.parseMamPageInfo(from: element)
        let page = XMPPArchivePage(messages: pendingArchiveQuery.messages, pageInfo: pageInfo)
        self.pendingArchiveQuery = nil
        pendingArchiveQuery.timeoutTask?.cancel()
        pendingArchiveQuery.continuation.resume(returning: page)
        return true
    }

    private func resumePendingArchiveQuery(with error: Error) {
        guard let pendingArchiveQuery else {
            return
        }

        self.pendingArchiveQuery = nil
        pendingArchiveQuery.timeoutTask?.cancel()
        pendingArchiveQuery.continuation.resume(throwing: error)
    }

    private func handleArchiveQueryTimeout(id: String, roomJID: String) {
        guard let pendingArchiveQuery,
              pendingArchiveQuery.id == id,
              pendingArchiveQuery.roomJID == roomJID else {
            return
        }

        resumePendingArchiveQuery(
            with: XMPPServiceError.timeout("Timed out loading message history for \(roomJID).")
        )
    }

    private func resumeAllPendingIQs(with error: Error) {
        let continuations = pendingIQResponses.values
        pendingIQResponses.removeAll()
        for continuation in continuations {
            continuation.resume(throwing: error)
        }
    }
}
