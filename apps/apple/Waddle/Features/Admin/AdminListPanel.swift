import SwiftUI
import WaddleKit

/// One list of the console: its rows a page at a time, or why there are
/// none to show.
struct AdminListPanel<Row: Identifiable & Sendable, RowView: View>: View {
    let list: AdminList<Row>
    let section: AdminSection
    /// Pull to refresh: the list, and what goes with it.
    let refresh: () async -> Void
    @ViewBuilder let row: (Row) -> RowView

    var body: some View {
        ScrollView {
            VStack(alignment: .trailing, spacing: Theme.Spacing.l) {
                if list.failure != nil, !list.rows.isEmpty {
                    refreshNotice
                }
                if !list.rows.isEmpty {
                    rows
                }
                if list.hasMore {
                    AdminLoadMore(isLoading: list.isLoadingMore, failed: list.moreFailure != nil) {
                        Task { await list.loadMore() }
                    }
                }
            }
            .padding([.horizontal, .bottom], Theme.Spacing.l)
            .frame(maxWidth: AdminConsoleLayout.readableWidth)
            .frame(maxWidth: .infinity)
        }
        .refreshable { await refresh() }
        .overlay {
            if let notice = AdminListNotice(
                isEmpty: list.rows.isEmpty,
                hasLoaded: list.hasLoaded,
                failure: list.failure,
                query: list.query
            ) {
                AdminListNoticeView(notice: notice, section: section, query: list.query, space: list.space)
                    .allowsHitTesting(false)
            }
        }
    }

    private var rows: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous)
        return LazyVStack(spacing: 0) {
            ForEach(list.rows) { item in
                row(item)
                    .overlay(alignment: .top) {
                        if item.id != list.rows.first?.id {
                            HuddleDivider()
                                .allowsHitTesting(false)
                        }
                    }
            }
        }
        .background(Theme.Colors.surface)
        .clipShape(shape)
        .overlay {
            shape.strokeBorder(Theme.Colors.borderSoft, lineWidth: WaddleTokens.BorderWidth.hair)
                .allowsHitTesting(false)
        }
    }

    private var refreshNotice: some View {
        Label("Couldn't refresh. This list may be out of date.", systemImage: "exclamationmark.triangle")
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.dangerText)
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.s)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.Colors.dangerSubtle, in: RoundedRectangle(cornerRadius: Theme.Radius.medium))
    }
}

/// Asks for the next page; says so when the last try failed.
private struct AdminLoadMore: View {
    let isLoading: Bool
    let failed: Bool
    let action: () -> Void

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            if failed {
                Text("Couldn't load more. Try again.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            }
            Button(isLoading ? "Loading…" : "Load more", action: action)
                .buttonStyle(.huddle(size: .small))
                .disabled(isLoading)
        }
        .frame(maxWidth: .infinity)
    }
}
