import Foundation

/// Matching a typed `/prefix` against the built-ins and the server's
/// extension commands. Built-ins shadow extension commands that claim one
/// of their names or aliases.
public enum SlashCandidates {
    /// Popover rows: matching built-ins first (by name or alias, each once),
    /// then eligible extension commands. An empty prefix lists everything.
    public static func filter(prefix: String, extensions: [ExtensionCommand], inRoom: Bool) -> [SlashCandidate] {
        let needle = prefix.lowercased()
        let builtins = BuiltinSlashCommand.allCases
            .filter { $0.keywords.contains { $0.hasPrefix(needle) } }
            .map(SlashCandidate.builtin)
        let matching = eligible(extensions, inRoom: inRoom).filter { command in
            composerKeyword(command).map { $0.hasPrefix(needle) } ?? false
        }
        return builtins + matching.map(SlashCandidate.extension)
    }

    /// Exact, case-insensitive resolution for submit. A built-in always
    /// wins; an extension command resolves only when exactly one matches,
    /// so an ambiguous prefix makes the user pick from the popover.
    public static func resolve(prefix: String, extensions: [ExtensionCommand], inRoom: Bool) -> SlashResolution? {
        if let builtin = BuiltinSlashCommand(keyword: prefix) { return .builtin(builtin) }
        let needle = prefix.lowercased()
        guard !needle.isEmpty else { return nil }
        let matches = eligible(extensions, inRoom: inRoom).filter { composerKeyword($0) == needle }
        guard matches.count == 1, let command = matches.first else { return nil }
        return .extension(command)
    }

    /// Commands with a composer prefix that may run here and that no
    /// built-in shadows.
    private static func eligible(_ extensions: [ExtensionCommand], inRoom: Bool) -> [ExtensionCommand] {
        extensions.filter { command in
            guard let keyword = composerKeyword(command) else { return false }
            if command.scope == .channel, !inRoom { return false }
            return BuiltinSlashCommand(keyword: keyword) == nil
        }
    }

    private static func composerKeyword(_ command: ExtensionCommand) -> String? {
        guard let prefix = command.composerPrefix, !prefix.isEmpty else { return nil }
        return prefix.lowercased()
    }
}
