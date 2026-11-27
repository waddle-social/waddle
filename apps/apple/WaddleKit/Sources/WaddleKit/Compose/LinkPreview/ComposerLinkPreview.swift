import Foundation
import Observation

/// Drives one draft's link preview with an injectable lookup and clock.
/// The underlying draft transitions are independent of tasks and timers.
@MainActor
@Observable
public final class ComposerLinkPreview {
    public enum State: Equatable, Sendable {
        case idle
        case loading(LinkPreviewURL)
        case ready(LinkPreviewURL, LinkPreviewPayload)
        case failed(LinkPreviewURL)
        case unsupported(LinkPreviewURL)
        case dismissed(LinkPreviewURL)

        var isLoading: Bool {
            if case .loading = self { true } else { false }
        }

        var isReady: Bool {
            if case .ready = self { true } else { false }
        }

        var isDismissed: Bool {
            if case .dismissed = self { true } else { false }
        }
    }

    public typealias Lookup = @MainActor (LinkPreviewRequest) async throws -> LinkPreviewLookup
    public typealias Sleep = @MainActor @Sendable (Duration) async throws -> Void

    // LOOKUP_DEBOUNCE_MS and SEND_LOOKUP_GRACE_MS in chat/src/lib/use-composer-link-preview.ts.
    nonisolated static let lookupDebounce: Duration = .milliseconds(400)
    nonisolated static let sendLookupGrace: Duration = .milliseconds(2_250)
    // LOOKUP_TIMEOUT_MS in chat/src/lib/xmpp/link-preview.ts.
    nonisolated static let lookupDeadline: Duration = .seconds(8)

    private var draft = LinkPreviewDraft()
    @ObservationIgnored private let now: @MainActor () -> Date
    @ObservationIgnored private let sleep: Sleep
    @ObservationIgnored private var pendingLookup: Lookup?
    @ObservationIgnored private var debounce: Task<Void, Never>?
    @ObservationIgnored private var lookupTask: Task<Void, Never>?
    @ObservationIgnored private var lookupTimeout: Task<Void, Never>?
    @ObservationIgnored private var sendWaiters: [UUID: AsyncStream<Void>.Continuation] = [:]

    public init(
        now: @escaping @MainActor () -> Date = Date.init,
        sleep: @escaping Sleep = { try await Task.sleep(for: $0) }
    ) {
        self.now = now
        self.sleep = sleep
    }

    public var state: State { draft.state }

    public func update(body: String, scopeJid: BareJID, lookup: @escaping Lookup) {
        let previous = draft.attempt
        let next = draft.update(body: body, scopeJid: scopeJid)
        guard previous != draft.attempt else { return }
        stopWork()
        guard let next else { return }
        pendingLookup = lookup
        debounce = Task { [weak self, sleep] in
            try? await sleep(Self.lookupDebounce)
            guard !Task.isCancelled else { return }
            self?.startLookup(next)
        }
    }

    public func reset() {
        draft.reset()
        stopWork()
    }

    public func dismiss() {
        draft.dismiss()
        stopWork()
    }

    /// A send ends the typing debounce. It can wait briefly for the active
    /// lookup, but a slow origin must never hold the send for the full timeout.
    public func payloadForSend(body: String, scopeJid: BareJID) async -> LinkPreviewPayload? {
        guard let attempt = draft.attempt, attempt.request.scopeJid == scopeJid,
              attempt.request.url == Self.firstEligibleURL(in: body) else { return nil }
        if state.isLoading {
            startLookup(attempt)
            guard await waitForLookup(), !Task.isCancelled else { return nil }
        }
        return draft.payload(for: attempt, at: now())
    }

    private func startLookup(_ attempt: LinkPreviewDraft.Attempt) {
        guard let lookup = pendingLookup else { return }
        pendingLookup = nil
        debounce?.cancel()
        debounce = nil
        lookupTask = Task { [weak self] in
            let result = (try? await lookup(attempt.request)) ?? .failed
            self?.finishLookup(result, for: attempt)
        }
        // UniFFI can finish after Swift cancellation. Invalidate the attempt
        // at the web lookup deadline without waiting for the FFI call.
        lookupTimeout = Task { [weak self, sleep] in
            try? await sleep(Self.lookupDeadline)
            guard !Task.isCancelled else { return }
            self?.finishLookup(.failed, for: attempt)
        }
    }

    private func finishLookup(_ result: LinkPreviewLookup, for attempt: LinkPreviewDraft.Attempt) {
        if draft.resolve(result, for: attempt) { stopWork() }
    }

    private func waitForLookup() async -> Bool {
        let id = UUID()
        let signal = AsyncStream<Void>.makeStream()
        sendWaiters[id] = signal.continuation
        defer { sendWaiters[id] = nil }
        return await withTaskGroup(of: Bool.self) { group in
            group.addTask {
                var events = signal.stream.makeAsyncIterator()
                _ = await events.next()
                return true
            }
            group.addTask { [sleep] in
                try? await sleep(Self.sendLookupGrace)
                return false
            }
            let settled = await group.next() ?? false
            group.cancelAll()
            return settled
        }
    }

    private func stopWork() {
        pendingLookup = nil
        debounce?.cancel()
        debounce = nil
        lookupTask?.cancel()
        lookupTask = nil
        lookupTimeout?.cancel()
        lookupTimeout = nil
        for waiter in sendWaiters.values { waiter.finish() }
        sendWaiters.removeAll()
    }
}
