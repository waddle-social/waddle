import Foundation
import Observation

/// Who is composing in each conversation (XEP-0085). Entries expire so a
/// lost `paused` never leaves a ghost indicator.
@MainActor
@Observable
public final class TypingStore {
    public private(set) var composing: [ConversationID: [String]] = [:]

    @ObservationIgnored private var expiries: [ConversationID: [String: Date]] = [:]
    @ObservationIgnored private let lifetime: TimeInterval

    public init(lifetime: TimeInterval = 30) {
        self.lifetime = lifetime
    }

    public func names(in conversation: ConversationID) -> [String] {
        composing[conversation] ?? []
    }

    public func apply(_ state: ChatState, from name: String, in conversation: ConversationID, now: Date = Date()) {
        if state == .composing {
            expiries[conversation, default: [:]][name] = now.addingTimeInterval(lifetime)
        } else {
            expiries[conversation]?[name] = nil
        }
        publish(conversation)
    }

    /// A real message from `name` ends their composing state.
    public func messageArrived(from name: String, in conversation: ConversationID) {
        guard expiries[conversation]?[name] != nil else { return }
        expiries[conversation]?[name] = nil
        publish(conversation)
    }

    /// Drops expired entries; returns true while anyone is still composing.
    @discardableResult
    public func sweep(now: Date = Date()) -> Bool {
        for conversation in Array(expiries.keys) {
            let before = expiries[conversation]?.count ?? 0
            expiries[conversation] = expiries[conversation]?.filter { $0.value > now }
            if (expiries[conversation]?.count ?? 0) != before {
                publish(conversation)
            }
        }
        return expiries.values.contains { !$0.isEmpty }
    }

    public func clear() {
        expiries.removeAll()
        composing.removeAll()
    }

    private func publish(_ conversation: ConversationID) {
        let names = (expiries[conversation] ?? [:]).keys.sorted()
        composing[conversation] = names.isEmpty ? nil : names
    }
}
