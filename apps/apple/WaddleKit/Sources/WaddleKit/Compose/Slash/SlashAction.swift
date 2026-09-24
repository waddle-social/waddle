import Foundation

/// What submitting a resolved slash command does.
public enum SlashAction: Hashable, Sendable {
    /// Send this draft text instead of what was typed.
    case send(String)
    /// Open the GIF picker searching for `query` (may be empty: trending).
    case searchGIFs(query: String)
    /// Set the account's manual presence.
    case setAvailability(Availability)
    /// Run a server extension command (XEP-0050).
    case runExtension(ExtensionCommand, ExtensionInvocation)
    /// A required argument is missing: complete the command, do not send.
    case incomplete
}
