import SwiftUI
import WaddleKit

/// A list inside a sheet's form: its rows, what it says while loading,
/// when empty or when it failed, and a way to load more.
///
/// It is one row of the form whatever it holds. A grouped form on macOS
/// draws its cards wrongly when the number of rows in a section changes
/// after a load, so the rows are stacked here.
struct AdminSheetList<Row: Identifiable & Sendable, RowView: View>: View {
    let list: AdminList<Row>
    /// What the rows are, for "Couldn't refresh the …".
    let noun: String
    let emptyText: String
    @ViewBuilder let row: (Row) -> RowView

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            if list.failure != nil {
                HStack {
                    Text(list.rows.isEmpty ? "Couldn't load the \(noun)." : "Couldn't refresh the \(noun).")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.dangerText)
                    Spacer(minLength: Theme.Spacing.xs)
                    Button("Try again") { Task { await list.reload() } }
                        .buttonStyle(.huddle(size: .small))
                        .disabled(list.isLoading)
                }
            } else if list.rows.isEmpty {
                Text(list.hasLoaded ? emptyText : "Loading…")
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            ForEach(Array(list.rows.enumerated()), id: \.element.id) { index, item in
                if index > 0 || list.failure != nil { HuddleDivider() }
                row(item)
            }
            if list.hasMore {
                Button(list.isLoadingMore ? "Loading…" : "Load more") {
                    Task { await list.loadMore() }
                }
                .buttonStyle(.huddle(size: .small))
                .disabled(list.isLoadingMore)
            }
            if list.moreFailure != nil {
                Text("Couldn't load more. Try again.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
