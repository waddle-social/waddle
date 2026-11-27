import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct ComposerLinkPreviewTests {
    private let first = "https://example.org/guide"
    private let second = "https://other.example/article"
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    @Test func firstEligibleURLMatchesTheWeb() {
        #expect(ComposerLinkPreview.firstEligibleURL(
            in: "http://example.org https://localhost https://bad.example:abc (HTTPS://example.org/guide), https://other.example/article"
        )?.rawValue == "HTTPS://example.org/guide")
        #expect(ComposerLinkPreview.firstEligibleURL(in: "http://example.org and https://localhost") == nil)
        #expect(ComposerLinkPreview.firstEligibleURL(in: "https://example.123 https://127.1")?.rawValue == "https://127.1")
        #expect(ComposerLinkPreview.firstEligibleURL(in: "https://[::ffff:192.0.2.1]/ https://example.org")?.rawValue == "https://example.org")
        #expect(ComposerLinkPreview.firstEligibleURL(in: "https://example。org")?.rawValue == "https://example。org")
        #expect(ComposerLinkPreview.firstEligibleURL(in: "https://foo.bar:999999999999999999999 https://valid.example")?.rawValue == "https://valid.example")
        #expect(ComposerLinkPreview.firstEligibleURL(in: "https://0x+1 https://valid.example")?.rawValue == "https://valid.example")
    }

    @Test func changingDraftDebouncesAndIgnoresStaleResults() {
        var preview = LinkPreviewDraft()
        let old = preview.update(body: first, scopeJid: bob)
        #expect(preview.state == .loading(LinkPreviewURL(rawValue: first)))
        #expect(preview.update(body: "words before \(first) and \(second)", scopeJid: bob) == nil)
        let next = preview.update(body: second, scopeJid: bob)
        preview.resolve(.ready(payload(first)), for: old!)
        #expect(preview.state == .loading(LinkPreviewURL(rawValue: second)))
        preview.resolve(.unsupported, for: next!)
        #expect(preview.state == .unsupported(LinkPreviewURL(rawValue: second)))
        _ = preview.update(body: "", scopeJid: bob)
        #expect(preview.state == .idle)
        #expect(preview.update(body: first, scopeJid: bob) != nil)
    }

    @Test func dismissalSticksWhileTheCandidateStaysActive() {
        var preview = LinkPreviewDraft()
        let attempt = preview.update(body: first, scopeJid: bob)!
        preview.dismiss()
        preview.resolve(.ready(payload(first)), for: attempt)
        #expect(preview.update(body: "more text \(first) \(second)", scopeJid: bob) == nil)
        #expect(preview.state == .dismissed(LinkPreviewURL(rawValue: first)))
        #expect(preview.update(body: second, scopeJid: bob) != nil)
        #expect(preview.update(body: first, scopeJid: bob) != nil)
        #expect(preview.update(body: first, scopeJid: roomConversation.jid) != nil)
    }

    @Test(arguments: [bobConversation, roomConversation])
    func debounceUsesTheFirstURLAndConversationAddress(conversation: ConversationID) async {
        let (session, port) = onlineSession()
        let clock = PreviewTestClock()
        var sleeps = clock.sleeps.makeAsyncIterator()
        let preview = ComposerLinkPreview(now: { now }, sleep: clock.sleep)
        port.linkPreviewResult = .success(.ready(payload(first)))
        port.linkPreviewLookups.isHolding = true
        preview.update(body: "see (\(first)), then \(second)", scopeJid: conversation.jid, lookup: session.lookupLinkPreview)
        let debounce = await sleeps.next()
        #expect(debounce?.duration == ComposerLinkPreview.lookupDebounce)
        #expect(port.linkPreviewRequests.isEmpty)
        debounce?.wake()

        let preparing = Task { await preview.payloadForSend(body: first, scopeJid: conversation.jid) }
        let grace = await sleeps.next()
        #expect(grace?.duration == ComposerLinkPreview.sendLookupGrace)
        port.linkPreviewLookups.isHolding = false
        port.linkPreviewLookups.release()
        let result = await preparing.value
        #expect(result == payload(first))
        #expect(port.linkPreviewRequests.map(\.url.rawValue) == [first])
        #expect(port.linkPreviewRequests.map(\.scopeJid) == [conversation.jid])
        #expect(preview.state == .ready(LinkPreviewURL(rawValue: first), payload(first)))
    }

    @Test func sendStartsDebouncedLookupAndCanLeaveWhileItIsStillHeld() async {
        let (session, port) = onlineSession()
        let clock = PreviewTestClock()
        var sleeps = clock.sleeps.makeAsyncIterator()
        let preview = ComposerLinkPreview(now: { now }, sleep: clock.sleep)
        port.linkPreviewLookups.isHolding = true
        preview.update(body: first, scopeJid: bob, lookup: session.lookupLinkPreview)
        let debounce = await sleeps.next()
        #expect(debounce?.duration == ComposerLinkPreview.lookupDebounce)
        let preparing = Task { await preview.payloadForSend(body: first, scopeJid: bob) }
        let grace = await sleeps.next()
        #expect(grace?.duration == ComposerLinkPreview.sendLookupGrace)
        grace?.wake()
        #expect(await preparing.value == nil)
        port.linkPreviewLookups.isHolding = false
        port.linkPreviewLookups.release()
    }

    @Test func dismissalWhileSendingDoesNotUseThePendingResult() async {
        let (session, port) = onlineSession()
        let clock = PreviewTestClock()
        var sleeps = clock.sleeps.makeAsyncIterator()
        let preview = ComposerLinkPreview(now: { now }, sleep: clock.sleep)
        port.linkPreviewLookups.isHolding = true
        port.linkPreviewResult = .success(.ready(payload(first)))
        preview.update(body: first, scopeJid: bob, lookup: session.lookupLinkPreview)
        _ = await sleeps.next()
        let preparing = Task { await preview.payloadForSend(body: first, scopeJid: bob) }
        let grace = await sleeps.next()
        #expect(grace?.duration == ComposerLinkPreview.sendLookupGrace)
        preview.dismiss()
        #expect(await preparing.value == nil)
        #expect(preview.state == .dismissed(LinkPreviewURL(rawValue: first)))
        port.linkPreviewLookups.isHolding = false
        port.linkPreviewLookups.release()
    }

    @Test func lookupHasItsOwnDeadline() async {
        let (session, port) = onlineSession()
        let clock = PreviewTestClock(reportsLookupTimeout: true)
        var sleeps = clock.sleeps.makeAsyncIterator()
        let preview = ComposerLinkPreview(now: { now }, sleep: clock.sleep)
        port.linkPreviewLookups.isHolding = true
        preview.update(body: first, scopeJid: bob, lookup: session.lookupLinkPreview)
        let debounce = await sleeps.next()
        #expect(debounce?.duration == ComposerLinkPreview.lookupDebounce)
        debounce?.wake()
        let lookupTimeout = await sleeps.next()
        #expect(lookupTimeout?.duration == ComposerLinkPreview.lookupDeadline)
        lookupTimeout?.wake()
        #expect(await preview.payloadForSend(body: first, scopeJid: bob) == nil)
        #expect(preview.state == .failed(LinkPreviewURL(rawValue: first)))
        port.linkPreviewLookups.isHolding = false
        port.linkPreviewLookups.release()
    }

    @Test func failedLookupsDoNotProvideAPreview() async {
        let (session, port) = onlineSession()
        let preview = ComposerLinkPreview(now: { now }, sleep: immediateSleep)
        port.linkPreviewResult = .failure(.timeout)
        preview.update(body: first, scopeJid: bob, lookup: session.lookupLinkPreview)
        #expect(await preview.payloadForSend(body: first, scopeJid: bob) == nil)
        #expect(preview.state == .failed(LinkPreviewURL(rawValue: first)))
    }

    @Test func sendsOnlyReadyFreshUndismissedTokens() async {
        let (session, port) = onlineSession()
        let preview = ComposerLinkPreview(now: { now }, sleep: immediateSleep)
        let future = LinkPreviewPayload(token: LinkPreviewToken(rawValue: "valid"), expiresAt: .distantFuture, preview: card(first))
        port.linkPreviewResult = .success(.ready(future))
        preview.update(body: first, scopeJid: bob, lookup: session.lookupLinkPreview)
        let draft = Draft(text: first)
        let sent = await session.send(draft, in: bobConversation, linkPreview: await preview.payloadForSend(body: first, scopeJid: bob))
        #expect(sent.flatMap { session.timelines.timeline(for: bobConversation).item(withID: $0) }?.message.linkPreviews == [card(first)])
        preview.dismiss()
        await session.send(draft, in: bobConversation, linkPreview: await preview.payloadForSend(body: first, scopeJid: bob))
        let expired = LinkPreviewPayload(token: LinkPreviewToken(rawValue: "expired"), expiresAt: now, preview: card(first))
        preview.update(body: "", scopeJid: bob, lookup: session.lookupLinkPreview)
        port.linkPreviewResult = .success(.ready(expired))
        preview.update(body: first, scopeJid: bob, lookup: session.lookupLinkPreview)
        await session.send(draft, in: bobConversation, linkPreview: await preview.payloadForSend(body: first, scopeJid: bob))
        #expect(port.sent.map { $0.options.linkPreviewToken?.rawValue } == ["valid", nil, nil])
        #expect(preview.state == .ready(LinkPreviewURL(rawValue: first), expired))
    }

    @Test func retryWithoutAuthorizationClearsTheProvisionalCard() async throws {
        let (session, port) = onlineSession()
        let authorization = LinkPreviewPayload(token: LinkPreviewToken(rawValue: "first-attempt"), expiresAt: .distantFuture, preview: card(first))
        let sent = await session.send(Draft(text: first), in: bobConversation, linkPreview: authorization)
        let id = try #require(sent)
        let timeline = session.timelines.timeline(for: bobConversation)
        #expect(timeline.item(withID: id)?.message.linkPreviews == [card(first)])

        session.sentMessageFailed(id, bounced: false)
        await session.retry(clientID: id)
        #expect(port.sent.map { $0.options.linkPreviewToken?.rawValue } == ["first-attempt", nil])
        #expect(timeline.item(withID: id)?.message.linkPreviews == [])
    }

    @Test func expiredQueuedPreviewIsOmittedAtDispatch() async {
        let (session, port) = onlineSession()
        let clock = PreviewTestClock()
        session.now = { clock.now }
        // A live send is already draining the queue.
        session.isFlushing = true
        let authorization = LinkPreviewPayload(token: LinkPreviewToken(rawValue: "short-lived"), expiresAt: now, preview: card(first))
        clock.now = now.addingTimeInterval(-1)
        await session.send(Draft(text: first), in: bobConversation, linkPreview: authorization)
        #expect(port.sent.isEmpty)
        #expect(session.outboundQueue.first?.options.linkPreviewToken?.rawValue == "short-lived")
        clock.now = now
        session.isFlushing = false
        await session.flushOutboundQueue()
        #expect(port.sent.count == 1)
        #expect(port.sent.first?.options.linkPreviewToken == nil)
        #expect(session.timelines.timeline(for: bobConversation).items.last?.message.linkPreviews == [])
    }

    @Test func editsKeepReplaceAndRemoveCards() async throws {
        let (session, port) = onlineSession()
        var incoming = directMessage(first, from: JID(bare: me.jid, resource: nil)!, to: JID(bare: bob, resource: nil)!, id: "original")
        incoming.linkPreviews = [card(first)]
        session.route(incoming)
        func row() throws -> TimelineItem {
            try #require(session.timelines.timeline(for: bobConversation).items.last)
        }

        let original = try row()
        let kept = LinkPreviewPayload(token: LinkPreviewToken(rawValue: "keep"), expiresAt: .distantFuture, preview: card(first))
        #expect(await session.edit(original, draft: Draft(text: "read \(first)"), linkPreview: kept))
        #expect(try row().message.linkPreviews == [card(first)])
        let updated = try row()
        let replaced = LinkPreviewPayload(token: LinkPreviewToken(rawValue: "replace"), expiresAt: .distantFuture, preview: card(second))
        #expect(await session.edit(updated, draft: Draft(text: second), linkPreview: replaced))
        #expect(try row().message.linkPreviews == [card(second)])
        let withReplacement = try row()
        #expect(await session.edit(withReplacement, draft: Draft(text: second)))
        #expect(try row().message.linkPreviews.isEmpty)
        #expect(port.corrections.map { $0.options.linkPreviewToken?.rawValue } == ["keep", "replace", nil])
    }

    @Test func receivedCorrectionsReplaceAndClearPreviewMetadata() throws {
        let (session, _) = onlineSession()
        let sender = JID(bare: bob, resource: nil)!
        let recipient = JID(bare: me.jid, resource: nil)!
        var original = directMessage(first, from: sender, to: recipient, id: "original")
        original.linkPreviews = [card(first)]
        session.route(original)
        var correction = directMessage(second, from: sender, to: recipient, id: "replace")
        correction.replacesID = "original"
        correction.linkPreviews = [card(second)]
        session.route(correction)
        let timeline = session.timelines.timeline(for: bobConversation)
        #expect(timeline.item(withID: "original")?.message.linkPreviews == [card(second)])

        var removal = directMessage(second, from: sender, to: recipient, id: "remove")
        removal.replacesID = "original"
        session.route(removal)
        #expect(timeline.item(withID: "original")?.message.linkPreviews == [])
    }

    @Test func outboxEncodingOmitsTheToken() throws {
        var options = OutboundOptions()
        options.linkPreview = payload(first)
        let encoded = try JSONEncoder().encode(options)
        #expect(!String(decoding: encoded, as: UTF8.self).contains("preview-token"))
        let restored = try JSONDecoder().decode(OutboundOptions.self, from: encoded)
        #expect(restored.linkPreviewToken == nil)
    }

    @Test func unchangedEditCanRemoveOrAddACardButDoesNotRenewIt() {
        let original = Draft(text: first)
        let samePreview = ComposerLinkPreviewEdit(original: original, originalPreview: card(first), draft: original, state: .ready(LinkPreviewURL(rawValue: first), payload(first)))
        #expect(samePreview.shouldResolve)
        #expect(!samePreview.shouldSend(payload(first)))
        let dismissed = ComposerLinkPreviewEdit(original: original, originalPreview: card(first), draft: original, state: .dismissed(LinkPreviewURL(rawValue: first)))
        #expect(dismissed.shouldSend(nil))
        let newPreview = ComposerLinkPreviewEdit(original: original, originalPreview: nil, draft: original, state: .ready(LinkPreviewURL(rawValue: first), payload(first)))
        #expect(newPreview.shouldSend(payload(first)))
        let pending = ComposerLinkPreviewEdit(original: original, originalPreview: nil, draft: original, state: .loading(LinkPreviewURL(rawValue: first)))
        #expect(!pending.shouldResolve)
    }

    @Test(arguments: ["https://example.org", "HTTPS://EXAMPLE.ORG:443"])
    func normalizedOriginalURLDoesNotMakeAnUnchangedEditRemoveOrReplaceACard(entered: String) {
        let canonical = "https://example.org/"
        let original = Draft(text: "Read \(entered)")
        let pending = ComposerLinkPreviewEdit(
            original: original, originalPreview: card(canonical), draft: original, state: .loading(LinkPreviewURL(rawValue: entered))
        )
        #expect(!pending.shouldResolve)
        #expect(!pending.shouldSend(nil))

        let ready = ComposerLinkPreviewEdit(
            original: original, originalPreview: card(canonical), draft: original, state: .ready(LinkPreviewURL(rawValue: entered), payload(entered))
        )
        #expect(!ready.shouldSend(payload(entered)))
    }

    private func onlineSession() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let session = SessionCoordinator(account: me, port: port)
        session.status.connection = .online
        session.isSendReady = true
        return (session, port)
    }

    private func card(_ url: String) -> LinkPreview {
        LinkPreview(url: URL(string: url)!, title: "A guide", summary: nil, imageURL: nil, imageAlt: nil, originalURL: url)
    }

    private func payload(_ url: String) -> LinkPreviewPayload {
        LinkPreviewPayload(token: LinkPreviewToken(rawValue: "preview-token"), expiresAt: now.addingTimeInterval(300), preview: card(url))
    }

    private func immediateSleep(_ duration: Duration) async throws {
        if duration != ComposerLinkPreview.lookupDebounce {
            let stream = AsyncStream<Void>.makeStream()
            for await _ in stream.stream { /* never yields: this sleep does not end */ }
        }
    }
}

/// Each timer is registered with the test and released by the test. A task
/// group can also cancel a losing timer through AsyncStream's iterator.
@MainActor
final class PreviewTestClock {
    struct Sleep: Sendable {
        let duration: Duration
        let wake: @Sendable () -> Void
    }

    var now = Date.distantPast
    private let reportsLookupTimeout: Bool
    private let reportsDebounce: Bool
    private let channel = AsyncStream<Sleep>.makeStream()

    init(reportsLookupTimeout: Bool = false, reportsDebounce: Bool = true) {
        self.reportsLookupTimeout = reportsLookupTimeout
        self.reportsDebounce = reportsDebounce
    }

    var sleeps: AsyncStream<Sleep> { channel.stream }

    func sleep(_ duration: Duration) async throws {
        let signal = AsyncStream<Void>.makeStream()
        if (reportsLookupTimeout || duration != ComposerLinkPreview.lookupDeadline),
           (reportsDebounce || duration != ComposerLinkPreview.lookupDebounce) {
            channel.continuation.yield(Sleep(duration: duration, wake: { signal.continuation.finish() }))
        }
        for await _ in signal.stream { /* waits for the test to finish the stream */ }
        try Task.checkCancellation()
    }
}
