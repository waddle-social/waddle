import Foundation
import Observation
import WaddleKit

/// The add people sheet: who to add, found by a name search, and how much
/// of the conversation they can read. The server makes each person a
/// member at once; it can still refuse one later, which is not shown.
@MainActor
@Observable
final class GroupAddPeopleModel {
    let room: BareJID
    /// Us and the people known to be in the group chat; never offered.
    private(set) var excluded: Set<BareJID>
    var query = ""
    private(set) var tokens: [RecipientToken]
    private var search: UserSearchState
    var history: GroupDMHistoryAccess = .fromJoin
    private(set) var isAdding = false
    private(set) var addError: String?

    /// `tokens` and `search` let a snapshot start with people picked.
    init(room: BareJID, excluded: Set<BareJID>, tokens: [RecipientToken] = [], search: UserSearchState = UserSearchState()) {
        self.room = room
        self.excluded = excluded
        self.tokens = tokens
        self.search = search
    }

    /// The latest search, without anyone already in the group chat.
    var visibleSearch: UserSearchState {
        var visible = search
        visible.results = search.results.filter { !excluded.contains($0.jid) }
        return visible
    }

    var canAdd: Bool {
        !tokens.isEmpty && !isAdding
    }

    /// Adds the member list to who is never offered, so people who are
    /// offline are left out too. If it cannot load, what is known stands.
    func excludeMembers(from load: () async throws -> [RoomMember]) async {
        guard let members = try? await load() else { return }
        excluded.formUnion(members.map(\.jid))
        tokens.removeAll { excluded.contains($0.jid) }
    }

    func toggle(_ token: RecipientToken) {
        let wasPicked = tokens.contains { $0.jid == token.jid }
        tokens = RecipientInput.toggled(token, in: tokens)
        if !wasPicked {
            query = ""
        }
    }

    /// Directory search, debounced: `.task(id:)` cancels the previous run
    /// on every keystroke, so only a pause of 300 ms reaches the server.
    func runSearch(for text: String, using session: SessionCoordinator) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            search = UserSearchState()
            return
        }
        try? await Task.sleep(nanoseconds: 300_000_000)
        guard !Task.isCancelled else { return }
        search.isSearching = true
        do {
            let found = try await session.searchUsers(trimmed)
            guard !Task.isCancelled else { return }
            search = UserSearchState(results: found, isSearching: false, errorMessage: nil, hasSearched: true)
        } catch {
            guard !Task.isCancelled else { return }
            search = UserSearchState(
                results: [],
                isSearching: false,
                errorMessage: ActionErrorCopy.message(for: error, fallback: "Search isn't available right now."),
                hasSearched: true
            )
        }
    }

    /// Adds each picked person in turn. Each one added is unpicked, so a
    /// failure leaves only the people still to add. True when all went.
    func add(using session: SessionCoordinator) async -> Bool {
        guard canAdd else { return false }
        isAdding = true
        addError = nil
        defer { isAdding = false }
        for person in tokens {
            do {
                try await session.inviteToGroupDM(room, invitee: person.jid, history: history)
                tokens.removeAll { $0.jid == person.jid }
            } catch {
                addError = ActionErrorCopy.message(for: error, fallback: "Couldn't add \(person.name). Try again.")
                return false
            }
        }
        return true
    }
}
