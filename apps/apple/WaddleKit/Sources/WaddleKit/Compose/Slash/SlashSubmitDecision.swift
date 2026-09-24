import Foundation

/// What pressing Send does with a draft that may be a slash command.
public enum SlashSubmitDecision: Hashable, Sendable {
    /// Not a command: send the draft as typed.
    case sendAsTyped
    /// A resolved command with everything it needs.
    case run(SlashAction)
    /// Complete the draft to this command and wait for more input: a
    /// command missing its argument (`/me`), or the only command a partial
    /// word matches (`/sh`).
    case complete(SlashCandidate)
    /// Several commands match (or none was typed): pick one from the list.
    case choose
    /// No command answers to `/<command>`.
    case unknown(command: String)

    public static func decide(text: String, extensions: [ExtensionCommand], inRoom: Bool) -> SlashSubmitDecision {
        guard let trigger = SlashTrigger.parse(text) else { return .sendAsTyped }
        if let resolution = SlashCandidates.resolve(prefix: trigger.prefix, extensions: extensions, inRoom: inRoom) {
            let action = resolution.action(trailing: trigger.trailing)
            return action == .incomplete ? .complete(candidate(for: resolution)) : .run(action)
        }
        let candidates = SlashCandidates.filter(prefix: trigger.prefix, extensions: extensions, inRoom: inRoom)
        if !trigger.prefix.isEmpty, candidates.isEmpty { return .unknown(command: trigger.prefix) }
        if !trigger.prefix.isEmpty, candidates.count == 1, let only = candidates.first { return .complete(only) }
        return .choose
    }

    private static func candidate(for resolution: SlashResolution) -> SlashCandidate {
        switch resolution {
        case let .builtin(command): return .builtin(command)
        case let .extension(command): return .extension(command)
        }
    }
}
