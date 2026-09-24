import Foundation

extension BuiltinSlashCommand {
    /// What submitting `/<command> <trailing>` does. Presence commands
    /// ignore any trailing text.
    public func action(trailing: String) -> SlashAction {
        let argument = trailing.trimmingCharacters(in: .whitespacesAndNewlines)
        switch self {
        case .me:
            // Canonical XEP-0245 prefix, so `/ME  waves` sends `/me waves`.
            return argument.isEmpty ? .incomplete : .send(MeAction.prefix + trailing)
        case .shrug:
            return .send(Shrug.appended(to: argument))
        case .giphy:
            return .searchGIFs(query: argument)
        case .away:
            return .setAvailability(.away)
        case .active:
            return .setAvailability(.available)
        case .dnd:
            return .setAvailability(.doNotDisturb)
        }
    }
}
