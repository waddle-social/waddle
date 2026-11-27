import Foundation

/// The first link and its lookup state. Request identity makes a late reply
/// harmless even when the same URL becomes active again.
struct LinkPreviewDraft {
    struct Attempt: Equatable {
        let request: LinkPreviewRequest
        let generation: Int
    }

    private(set) var state: ComposerLinkPreview.State = .idle
    private(set) var attempt: Attempt?
    private var generation = 0

    @discardableResult
    mutating func update(body: String, scopeJid: BareJID) -> Attempt? {
        guard let url = ComposerLinkPreview.firstEligibleURL(in: body) else {
            reset()
            return nil
        }
        let request = LinkPreviewRequest(url: url, scopeJid: scopeJid)
        guard attempt?.request != request else { return nil }
        generation += 1
        let next = Attempt(request: request, generation: generation)
        attempt = next
        state = .loading(url)
        return next
    }

    mutating func reset() {
        attempt = nil
        state = .idle
    }

    mutating func dismiss() {
        guard let attempt else { return }
        state = .dismissed(attempt.request.url)
    }

    @discardableResult
    mutating func resolve(_ result: LinkPreviewLookup, for attempt: Attempt) -> Bool {
        guard self.attempt == attempt, state.isLoading else { return false }
        let url = attempt.request.url
        switch result {
        case let .ready(payload): state = .ready(url, payload)
        case .unsupported, .blocked: state = .unsupported(url)
        case .failed: state = .failed(url)
        }
        return true
    }

    func payload(for attempt: Attempt, at now: Date) -> LinkPreviewPayload? {
        guard self.attempt == attempt, case let .ready(_, payload) = state else { return nil }
        return payload.ifFresh(at: now)
    }
}
