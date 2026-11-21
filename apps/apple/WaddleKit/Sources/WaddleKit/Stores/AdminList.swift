import Foundation
import Observation

/// One list of the admin console: searched by the start of a name and
/// loaded a page at a time.
@MainActor
@Observable
public final class AdminList<Row: Identifiable & Sendable> {
    typealias Fetch = @MainActor (_ prefix: String?, _ cursor: String?) async throws -> AdminPage<Row>

    public private(set) var rows: [Row] = []
    public private(set) var isLoading = false
    public private(set) var isLoadingMore = false
    /// The text the rows, or the lack of them, answer.
    public private(set) var query = ""
    /// The server has answered `query`.
    public private(set) var hasLoaded = false
    /// Why the last search or refresh failed; `rejected` when the account
    /// may not use the console.
    public private(set) var failure: PortError?
    /// Why the last "load more" failed.
    public private(set) var moreFailure: PortError?
    public var hasMore: Bool { cursor != nil }

    @ObservationIgnored private let fetch: Fetch
    /// The text last asked for, answered or not.
    @ObservationIgnored private var requested = ""
    @ObservationIgnored private var cursor: String?
    /// Counts searches; an answer to an earlier one is dropped.
    @ObservationIgnored private var serial = 0

    init(fetch: @escaping Fetch) {
        self.fetch = fetch
    }

    /// Shows the first page of what starts with `text`; everything when it
    /// is blank. A refresh that fails keeps the rows shown; a search for
    /// other text that fails shows none, since they answered other text.
    /// A search whose task was cancelled changes nothing.
    public func load(matching text: String = "") async {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        serial += 1
        let search = serial
        requested = text
        isLoading = true
        isLoadingMore = false
        moreFailure = nil
        let result = await Self.attempt { try await self.fetch(text.isEmpty ? nil : text, nil) }
        guard search == serial else { return }
        isLoading = false
        guard !Task.isCancelled else { return }
        switch result {
        case let .success(page):
            rows = page.rows
            cursor = page.nextCursor
            hasLoaded = true
            failure = nil
        case let .failure(error):
            if text != query {
                rows = []
                cursor = nil
                hasLoaded = false
            }
            failure = error
        }
        query = text
    }

    /// Asks again for the text last searched for, answered yet or not.
    public func reload() async {
        await load(matching: requested)
    }

    /// Adds the next page, if there is one and it is not on its way.
    public func loadMore() async {
        guard let cursor, !isLoading, !isLoadingMore else { return }
        let search = serial
        isLoadingMore = true
        moreFailure = nil
        let result = await Self.attempt { try await self.fetch(self.query.isEmpty ? nil : self.query, cursor) }
        guard search == serial else { return }
        isLoadingMore = false
        guard !Task.isCancelled else { return }
        switch result {
        case let .success(page):
            // The list may have changed between the two pages.
            let shown = Set(rows.map(\.id))
            rows += page.rows.filter { !shown.contains($0.id) }
            self.cursor = page.nextCursor
        case let .failure(error):
            moreFailure = error
        }
    }

    /// Shows a row as the server now holds it, where it was.
    public func replace(_ row: Row) {
        dropAnswersOnTheirWay()
        guard let index = rows.firstIndex(where: { $0.id == row.id }) else { return }
        rows[index] = row
    }

    /// Takes out a row the server no longer has.
    public func remove(_ id: Row.ID) {
        dropAnswersOnTheirWay()
        rows.removeAll { $0.id == id }
    }

    /// A page asked for before a change may not know of it, so its answer
    /// is dropped rather than shown over the change.
    private func dropAnswersOnTheirWay() {
        serial += 1
        isLoading = false
        isLoadingMore = false
    }

    private static func attempt(_ fetch: () async throws -> AdminPage<Row>) async -> Result<AdminPage<Row>, PortError> {
        do {
            return .success(try await fetch())
        } catch {
            return .failure(error as? PortError ?? .failed)
        }
    }
}
