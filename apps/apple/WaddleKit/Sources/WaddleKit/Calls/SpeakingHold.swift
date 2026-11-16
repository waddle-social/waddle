import Observation

/// Who the call surface shows speaking. Speech has gaps between words, so
/// only the off-transition is debounced, per person: someone who starts
/// speaking shows at once, someone who stops stays for the hold and is kept
/// if they speak again within it (Android's `releasingAfter`).
@MainActor @Observable public final class SpeakingHold {
    public nonisolated static let defaultHold: Duration = .milliseconds(300)

    public private(set) var shown: Set<BareJID> = []
    @ObservationIgnored private let hold: Duration
    @ObservationIgnored private var latest: Set<BareJID> = []
    @ObservationIgnored private var releases: [BareJID: Task<Void, Never>] = [:]

    public init(hold: Duration = SpeakingHold.defaultHold) {
        self.hold = hold
    }

    /// Takes the newest set of people speaking.
    public func update(_ speaking: Set<BareJID>) {
        latest = speaking
        for person in speaking { releases.removeValue(forKey: person)?.cancel() }
        let quiet = shown.subtracting(speaking).subtracting(releases.keys)
        publish(shown.union(speaking))
        for person in quiet {
            releases[person] = Task { [weak self, hold] in
                try? await Task.sleep(for: hold)
                guard !Task.isCancelled else { return }
                self?.release(person)
            }
        }
    }

    /// The hold ran out: drop `person` unless they spoke again.
    private func release(_ person: BareJID) {
        releases[person] = nil
        if !latest.contains(person) { publish(shown.subtracting([person])) }
    }

    private func publish(_ next: Set<BareJID>) {
        if shown != next { shown = next }
    }
}
