import Foundation
import Testing
@testable import WaddleKit

/// A message with every outbound option populated.
private func fullMessage() -> OutboundMessage {
    let encrypted = EncryptedFileSource(
        cipher: .aes256GCM,
        keyBase64: "a2V5",
        ivBase64: "aXY=",
        digests: FileDigests([.sha256: Data("hash".utf8)]),
        sources: [URL(string: "https://upload.waddle.test/enc")!]
    )
    let file = SharedFile(
        url: URL(string: "https://upload.waddle.test/cat.png")!,
        name: "cat.png",
        mediaType: "image/png",
        size: 1234,
        width: 640,
        height: 480,
        description: "a cat",
        disposition: .inline,
        digests: FileDigests([.sha512: Data("plain".utf8)]),
        encrypted: encrypted
    )
    let options = OutboundOptions(
        reply: .init(targetID: "s1", author: jid("general@muc.waddle.test/bob"), fallback: 0..<6),
        thread: "t1",
        markupSpans: [
            MarkupSpan(kind: .bold, start: 0, end: 1),
            MarkupSpan(kind: .italic, start: 1, end: 2),
            MarkupSpan(kind: .strikethrough, start: 2, end: 3),
            MarkupSpan(kind: .code, start: 3, end: 4),
            MarkupSpan(kind: .codeBlock, start: 4, end: 5),
            MarkupSpan(kind: .blockquote, start: 5, end: 6),
            MarkupSpan(kind: .link(URL(string: "https://waddle.social")!), start: 6, end: 7),
        ],
        references: [
            Reference(kind: .mention, uri: "xmpp:bob@waddle.test", begin: 0, end: 4),
            Reference(kind: .data, uri: "https://waddle.social", begin: 5, end: 6),
            Reference(kind: .other("custom"), uri: "urn:x", begin: 6, end: 7),
        ],
        sharedFiles: [file],
        requestDisplayedMarker: true
    )
    return OutboundMessage(clientID: "c-full", conversation: roomConversation, body: "> hi\n\nyo there", options: options)
}

private func temporaryOutboxURL() -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("waddle-outbox-tests-\(UUID().uuidString)", isDirectory: true)
        .appendingPathComponent("nested", isDirectory: true)
        .appendingPathComponent("outbox.json")
}

@Suite("Outbox encoding")
struct OutboxEncodingTests {
    @Test func outboundMessageRoundTripsWithEveryOption() throws {
        let entry = PersistedOutbound(message: fullMessage(), createdAt: date(42), state: .failed)
        let data = try OutboxFile.encode([entry])
        #expect(OutboxFile.decode(data) == .success([entry]))
    }

    @Test func invalidJIDIsRejectedOnDecode() throws {
        let message = OutboundMessage(clientID: "c1", conversation: bobConversation, body: "hi", options: OutboundOptions())
        let valid = try OutboxFile.encode([PersistedOutbound(message: message, createdAt: date(0), state: .pending)])
        let json = try #require(String(data: valid, encoding: .utf8))
        #expect(json.contains("\"bob@waddle.test\""))
        let invalid = json.replacingOccurrences(of: "\"bob@waddle.test\"", with: "\"a@b@c\"")
        #expect(OutboxFile.decode(Data(invalid.utf8)) == .failure(.unreadable))
    }

    @Test func unknownVersionIsReported() {
        #expect(OutboxFile.decode(Data(#"{"version":99,"entries":[]}"#.utf8)) == .failure(.unknownVersion(99)))
    }

    @Test func accountKeysAreFileSafeAndDistinct() {
        #expect(FileOutboxStore.fileKey(for: bare("alice@waddle.test")) == "alice_40waddle.test")
        #expect(FileOutboxStore.fileKey(for: bare("a_b@x.test")) != FileOutboxStore.fileKey(for: bare("a@b_x.test")))
        let base = URL(fileURLWithPath: "/support", isDirectory: true)
        #expect(FileOutboxStore.url(for: bare("alice@waddle.test"), in: base).path == "/support/Waddle/Outbox/alice_40waddle.test.json")
    }
}

@MainActor
@Suite("File outbox store")
struct FileOutboxStoreTests {
    @Test func savesAndLoadsInANewDirectory() throws {
        let store = FileOutboxStore(url: temporaryOutboxURL())
        #expect(try store.load().isEmpty)
        let entries = [
            PersistedOutbound(message: fullMessage(), createdAt: date(1), state: .pending),
            PersistedOutbound(
                message: OutboundMessage(clientID: "c2", conversation: bobConversation, body: "dm", options: OutboundOptions()),
                createdAt: date(2),
                state: .failed
            ),
        ]
        try store.save(entries)
        #expect(try FileOutboxStore(url: store.url).load() == entries)
    }

    @Test func emptySaveAndRemoveDeleteTheFile() throws {
        let store = FileOutboxStore(url: temporaryOutboxURL())
        let entry = PersistedOutbound(message: fullMessage(), createdAt: date(1), state: .pending)
        try store.save([entry])
        try store.save([])
        #expect(!FileManager.default.fileExists(atPath: store.url.path))
        try store.save([entry])
        store.remove()
        #expect(!FileManager.default.fileExists(atPath: store.url.path))
    }

    @Test func corruptFileIsDiscarded() throws {
        let store = FileOutboxStore(url: temporaryOutboxURL())
        try store.save([PersistedOutbound(message: fullMessage(), createdAt: date(1), state: .pending)])
        try Data("{not json".utf8).write(to: store.url)
        #expect(try store.load().isEmpty)
        #expect(!FileManager.default.fileExists(atPath: store.url.path))
    }

    @Test func unknownVersionFileIsDiscarded() throws {
        let store = FileOutboxStore(url: temporaryOutboxURL())
        try store.save([PersistedOutbound(message: fullMessage(), createdAt: date(1), state: .pending)])
        try Data(#"{"version":2,"entries":[]}"#.utf8).write(to: store.url)
        #expect(try store.load().isEmpty)
        #expect(!FileManager.default.fileExists(atPath: store.url.path))
    }
}

/// Fails to load until `isAvailable`, like a file before first unlock.
@MainActor
private final class LockedOutboxStore: OutboxStore {
    struct Locked: Error {}
    var isAvailable = false
    var entries: [PersistedOutbound]
    var saveCount = 0

    init(entries: [PersistedOutbound]) {
        self.entries = entries
    }

    func load() throws -> [PersistedOutbound] {
        guard isAvailable else { throw Locked() }
        return entries
    }

    func save(_ entries: [PersistedOutbound]) throws {
        saveCount += 1
        self.entries = entries
    }

    func remove() {
        entries.removeAll()
    }
}

@MainActor
@Suite("Outbox across launches")
struct OutboxRestoreTests {
    private func started(_ store: any OutboxStore, port: FakePort = FakePort()) -> (SessionCoordinator, FakePort) {
        let coordinator = SessionCoordinator(account: me, port: port, outboxStore: store)
        coordinator.start()
        return (coordinator, port)
    }

    private func becomeReady(_ coordinator: SessionCoordinator) async {
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        await coordinator.flushOutboundQueue()
    }

    @Test func offlineSendSurvivesAKillAndFlushesUnderItsID() async throws {
        let store = InMemoryOutboxStore()
        let (first, _) = started(store)
        let id = try #require(await first.send(Draft(text: "later"), in: bobConversation))
        let created = try #require(first.timelines.timeline(for: bobConversation).item(withID: id)).receivedAt
        #expect(store.entries.map(\.state) == [.pending])

        let (second, port) = started(store)
        let echo = try #require(second.timelines.timeline(for: bobConversation).item(withID: id))
        #expect(echo.isLocalEcho)
        #expect(echo.body == "later")
        #expect(echo.receivedAt == created)
        #expect(second.deliveries.state(of: id) == .queued)
        #expect(second.outboundQueue.map(\.clientID) == [id])

        await becomeReady(second)
        #expect(port.sent.map(\.clientID) == [id])
        // Written but unconfirmed: still re-sent if the app dies now.
        #expect(store.entries.map(\.message.clientID) == [id])
        second.handle(.deliveryAcked(stanzaID: id))
        #expect(store.entries.isEmpty)
    }

    @Test func restoredEchoesKeepComposeOrder() async throws {
        let store = InMemoryOutboxStore()
        let (first, _) = started(store)
        let one = try #require(await first.send(Draft(text: "one"), in: roomConversation))
        let two = try #require(await first.send(Draft(text: "two"), in: roomConversation))

        let (second, port) = started(store)
        #expect(second.timelines.timeline(for: roomConversation).items.map(\.id) == [one, two])
        await becomeReady(second)
        #expect(port.sent.map(\.clientID) == [one, two])
    }

    @Test func unacknowledgedSendIsResentUnderTheSameID() async throws {
        let store = InMemoryOutboxStore()
        let (first, firstPort) = started(store)
        await becomeReady(first)
        let id = try #require(await first.send(Draft(text: "in flight"), in: bobConversation))
        #expect(firstPort.sent.map(\.clientID) == [id])
        #expect(first.deliveries.state(of: id) == .sent)

        let (second, port) = started(store)
        #expect(second.deliveries.state(of: id) == .queued)
        await becomeReady(second)
        #expect(port.sent.map(\.clientID) == [id])
    }

    @Test func roomReflectionConfirmsAnUnackedSend() async throws {
        let store = InMemoryOutboxStore()
        let (coordinator, _) = started(store)
        await becomeReady(coordinator)
        let id = try #require(await coordinator.send(Draft(text: "hello"), in: roomConversation))
        #expect(store.entries.count == 1)
        coordinator.route(roomMessage("hello", from: "alice", stanzaID: "room-1", originID: id))
        #expect(store.entries.isEmpty)
    }

    @Test func failedSendSurvivesAsFailedAndCanBeRetried() async throws {
        let store = InMemoryOutboxStore()
        let failing = FakePort()
        failing.sendOutcome = { _ in .rejected }
        let (first, _) = started(store, port: failing)
        await becomeReady(first)
        let id = try #require(await first.send(Draft(text: "nope"), in: roomConversation))
        #expect(store.entries.map(\.state) == [.failed])

        let (second, port) = started(store)
        #expect(second.deliveries.state(of: id) == .failed)
        #expect(second.timelines.timeline(for: roomConversation).item(withID: id)?.isLocalEcho == true)
        await becomeReady(second)
        #expect(port.sent.isEmpty)
        await second.retry(clientID: id)
        #expect(port.sent.map(\.clientID) == [id])
    }

    @Test func streamFailureAfterWritingIsSavedAsFailed() async throws {
        let store = InMemoryOutboxStore()
        let (coordinator, _) = started(store)
        await becomeReady(coordinator)
        let id = try #require(await coordinator.send(Draft(text: "lost"), in: bobConversation))
        coordinator.handle(.deliveryFailed(stanzaID: id))
        #expect(store.entries.map(\.state) == [.failed])
    }

    @Test func discardRemovesTheSavedMessage() async throws {
        let store = InMemoryOutboxStore()
        let (coordinator, _) = started(store)
        let id = try #require(await coordinator.send(Draft(text: "never mind"), in: bobConversation))
        coordinator.discard(clientID: id, in: bobConversation)
        #expect(store.entries.isEmpty)
    }

    @Test func stopKeepsTheFileAndSignOutRemovesIt() async throws {
        let store = FileOutboxStore(url: temporaryOutboxURL())
        let (first, _) = started(store)
        let id = try #require(await first.send(Draft(text: "later"), in: bobConversation))
        await first.stop()
        #expect(try store.load().map(\.message.clientID) == [id])

        first.start()
        #expect(first.outboundQueue.map(\.clientID) == [id])
        await first.signOut()
        #expect(!FileManager.default.fileExists(atPath: store.url.path))
    }

    @Test func unreadableStorageIsNeverOverwrittenAndIsRetried() async throws {
        let saved = PersistedOutbound(
            message: OutboundMessage(clientID: "old", conversation: bobConversation, body: "old", options: OutboundOptions()),
            createdAt: date(1),
            state: .pending
        )
        let store = LockedOutboxStore(entries: [saved])
        let (coordinator, port) = started(store)
        let id = try #require(await coordinator.send(Draft(text: "new"), in: bobConversation))
        #expect(store.saveCount == 0)

        store.isAvailable = true
        coordinator.resume()
        #expect(coordinator.outboundQueue.map(\.clientID) == ["old", id])
        #expect(store.entries.map(\.message.clientID) == ["old", id])
        #expect(port.sent.isEmpty)
    }

    @Test func lateLoadFlushesWhenAlreadyReady() async throws {
        let saved = PersistedOutbound(
            message: OutboundMessage(clientID: "old", conversation: bobConversation, body: "old", options: OutboundOptions()),
            createdAt: date(1),
            state: .pending
        )
        let store = LockedOutboxStore(entries: [saved])
        let (coordinator, port) = started(store)
        coordinator.status.connection = .online
        coordinator.isSendReady = true

        store.isAvailable = true
        coordinator.resume()
        for _ in 0..<100 where port.sent.isEmpty {
            await Task.yield()
        }
        #expect(port.sent.map(\.clientID) == ["old"])
    }
}

/// Regressions from the state review.
@MainActor
@Suite("Outbox after a replayed failure")
struct OutboxReplayTests {
    /// XEP-0198 resume `<failed/>`: the core reports the send failed, replays
    /// it on the fresh stream, and it is acknowledged there.
    @Test func failedThenAckedIsNotRestoredAsFailed() async throws {
        let store = InMemoryOutboxStore()
        let first = SessionCoordinator(account: me, port: FakePort(), outboxStore: store)
        first.start()
        first.status.connection = .online
        first.isSendReady = true
        let id = try #require(await first.send(Draft(text: "hello"), in: bobConversation))
        first.handle(.deliveryFailed(stanzaID: id))
        first.handle(.deliveryAcked(stanzaID: id))
        #expect(store.entries.isEmpty)

        let second = SessionCoordinator(account: me, port: FakePort(), outboxStore: store)
        second.start()
        #expect(second.deliveries.state(of: id) != .failed)
        await first.stop()
        await second.stop()
    }

    /// A failed room send that the room reflects was delivered: its local
    /// echo is replaced, and it is not saved as failed.
    @Test func failedThenReflectedIsNotSavedAsFailed() async throws {
        let store = InMemoryOutboxStore()
        let coordinator = SessionCoordinator(account: me, port: FakePort(), outboxStore: store)
        coordinator.start()
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        let id = try #require(await coordinator.send(Draft(text: "hello"), in: roomConversation))
        coordinator.handle(.deliveryFailed(stanzaID: id))
        #expect(store.entries.map(\.state) == [.failed])
        coordinator.route(roomMessage("hello", from: me.nick, stanzaID: "s-own", originID: id))
        #expect(store.entries.isEmpty)
        await coordinator.stop()
    }
}
