import Foundation
import Observation
import WaddleKit

/// The add people sheet: who to invite, found by search or a typed
/// address, and how much of the conversation they can read.
@MainActor
@Observable
final class GroupInviteModel {
    let room: BareJID
    /// Us and the people known to be in the group chat; never offered.
    let excluded: Set<BareJID>
    var query = ""
    private(set) var tokens: [RecipientToken]
    private(set) var search: UserSearchState
    var history: GroupDMHistoryAccess = .fromJoin
    private(set) var isSending = false
    private(set) var sendError: String?

    /// `tokens` and `search` let a snapshot start with people picked.
    init(room: BareJID, excluded: Set<BareJID>, tokens: [RecipientToken] = [], search: UserSearchState = UserSearchState()) {
        self.room = room
        self.excluded = excluded
        self.tokens = tokens
        self.search = search
    }

    /// A typed address of someone not in the group chat yet.
    var typedAddress: BareJID? {
        RecipientInput.address(from: query).flatMap { excluded.contains($0) ? nil : $0 }
    }

    /// A typed address of someone already in the group chat.
    var typedMember: BareJID? {
        RecipientInput.address(from: query).flatMap { excluded.contains($0) ? $0 : nil }
    }

    var invitees: [RecipientToken] {
        RecipientInput.recipients(tokens: tokens, typedAddress: typedAddress)
    }

    var canSend: Bool {
        !invitees.isEmpty && !isSending
    }

    func toggle(_ token: RecipientToken) {
        let wasPicked = tokens.contains { $0.jid == token.jid }
        tokens = RecipientInput.toggled(token, in: tokens)
        if !wasPicked {
            query = ""
        }
    }

    /// Return in the field picks a typed address.
    func addTypedAddress() {
        guard let address = typedAddress else { return }
        if !tokens.contains(where: { $0.jid == address }) {
            tokens.append(RecipientToken(jid: address, name: nil))
        }
        query = ""
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
            search = UserSearchState(
                results: found.filter { !excluded.contains($0.jid) },
                isSearching: false,
                errorMessage: nil,
                hasSearched: true
            )
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

    /// Sends one invitation per person. Each one sent is unpicked, so a
    /// failure leaves only the people still to invite. True when all went.
    func send(using session: SessionCoordinator) async -> Bool {
        guard canSend else { return false }
        isSending = true
        sendError = nil
        defer { isSending = false }
        for invitee in invitees {
            do {
                try await session.inviteToGroupDM(room, invitee: invitee.jid, history: history)
                tokens.removeAll { $0.jid == invitee.jid }
            } catch {
                sendError = ActionErrorCopy.message(for: error, fallback: "Couldn't invite \(invitee.name). Try again.")
                return false
            }
        }
        return true
    }
}
