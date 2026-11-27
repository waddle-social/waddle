import Observation
import WaddleKit

/// Loads directory results for the active WaddleKit suggestion query.
@MainActor
@Observable
final class ComposerSuggestionDirectory {
    private(set) var results: ComposerSuggestionsModel.DirectoryResults?

    func search(_ query: ComposerSuggestionsModel.DirectoryQuery?, using session: SessionCoordinator) async {
        guard let query, !Task.isCancelled else { return }
        if let cached = session.cachedMentionableUsers(query.text, in: query.room) {
            results = .init(query: query, users: cached)
            return
        }
        do {
            try await Task.sleep(for: .milliseconds(200))
            let users = try await session.searchMentionableUsers(query.text, in: query.room)
            guard !Task.isCancelled else { return }
            results = .init(query: query, users: users)
        } catch {
            // Directory search is best effort; occupants are still offered.
        }
    }
}
