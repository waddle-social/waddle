import Foundation
import WaddleKit

/// A person picked in the new message sheet.
struct RecipientToken: Identifiable, Hashable {
    let jid: BareJID
    let name: String

    var id: BareJID { jid }

    init(jid: BareJID, name: String?) {
        self.jid = jid
        let trimmed = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.name = trimmed.isEmpty ? (jid.localpart ?? jid.domain) : trimmed
    }
}

/// Pure rules behind the new message sheet.
enum RecipientInput {
    /// A typed user address (`name@domain`) that can be added without a
    /// directory hit. A bare domain is a server, not a person.
    static func address(from query: String) -> BareJID? {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.contains("@"), let jid = BareJID(parsing: trimmed), jid.localpart != nil else { return nil }
        return jid
    }

    /// Adds `token`, or removes it when already picked.
    static func toggled(_ token: RecipientToken, in tokens: [RecipientToken]) -> [RecipientToken] {
        if tokens.contains(where: { $0.jid == token.jid }) {
            return tokens.filter { $0.jid != token.jid }
        }
        return tokens + [token]
    }

    /// Search results minus the signed-in account.
    static func visibleResults(_ results: [UserSearchResult], excluding account: BareJID) -> [UserSearchResult] {
        results.filter { $0.jid != account }
    }

    /// "Ana, Ben and Cy" or "Ana, Ben and 3 others".
    static func suggestedGroupName(for tokens: [RecipientToken], shown: Int = 2) -> String {
        let names = tokens.map(\.name)
        switch names.count {
        case 0: return ""
        case 1: return names[0]
        case 2: return "\(names[0]) and \(names[1])"
        default:
            let head = names.prefix(shown).joined(separator: ", ")
            let rest = names.count - shown
            if rest == 1, let last = names.last {
                return "\(head) and \(last)"
            }
            return "\(head) and \(rest) others"
        }
    }

    /// Picked tokens, or the typed address alone when nothing is picked.
    static func recipients(tokens: [RecipientToken], typedAddress: BareJID?) -> [RecipientToken] {
        guard tokens.isEmpty else { return tokens }
        return typedAddress.map { [RecipientToken(jid: $0, name: nil)] } ?? []
    }

    /// The typed name, else the suggestion.
    static func groupName(typed: String, tokens: [RecipientToken]) -> String {
        let trimmed = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? suggestedGroupName(for: tokens) : trimmed
    }
}
