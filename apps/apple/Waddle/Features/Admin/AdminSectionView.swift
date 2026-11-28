import SwiftUI
import WaddleKit

/// One section of the console: its list, searched by the text typed and
/// narrowed to a space where one is chosen, with the toolbar's refresh
/// and "new" buttons and the sheet a row opens.
struct AdminSectionView<Row: Identifiable & Sendable, RowView: View, Sheet: View>: View {
    @Environment(SessionCoordinator.self) private var session
    let section: AdminSection
    let list: AdminList<Row>
    let console: AdminConsole
    let query: String
    var space: AdminSpace?
    /// A row, and what choosing it does.
    @ViewBuilder let row: (Row, _ open: @escaping () -> Void) -> RowView
    @ViewBuilder let sheet: (AdminSheetSubject<Row>) -> Sheet
    @State private var subject: AdminSheetSubject<Row>?

    var body: some View {
        AdminListPanel(list: list, section: section, refresh: { await console.refresh(section) }) { item in
            row(item) { subject = .existing(item) }
        }
        .task(id: Search(query: query, space: space, isOnline: session.connection == .online)) {
            await search()
        }
        .toolbar {
            if let newTitle = section.copy.newTitle {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        subject = .new
                    } label: {
                        Label(newTitle, systemImage: "plus")
                    }
                    .buttonStyle(.huddle(.primary, size: .small))
                    .help(newTitle)
                }
            }
            ToolbarItem(placement: .primaryAction) {
                HuddleIconButton("Refresh", symbol: "arrow.clockwise") {
                    Task { await console.refresh(section) }
                }
                .foregroundStyle(Theme.Colors.fgMuted)
                .disabled(list.isLoading)
            }
        }
        .sheet(item: $subject, onDismiss: { console.markOthersStale(than: section) }) { subject in
            sheet(subject)
                .carryingCallDock()
        }
    }

    /// Asks the list for the text typed and the space chosen. A list that
    /// already answers them is left as it is, pages and all, when its
    /// section is shown again, unless it may be out of date.
    private func search() async {
        // Wanted at once, so a refresh before an answer asks for it.
        list.want(query, in: space)
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let answers = list.query == text && list.space?.id == space?.id
        if list.hasLoaded, list.failure == nil, answers, !console.stale.contains(section) { return }
        await list.search(query, in: space)
        if !Task.isCancelled { console.markFresh(section) }
    }

    /// What the list on screen answers: asked again when the text or the
    /// space changes and when the connection comes back.
    private struct Search: Hashable {
        let query: String
        let space: AdminSpace?
        let isOnline: Bool
    }
}

extension AdminList {
    /// Asks for what is typed, after a pause in typing; an empty field or
    /// another space asks at once. Typing on cancels a search not yet
    /// asked for.
    func search(_ text: String, in space: AdminSpace? = nil) async {
        want(text, in: space)
        if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, self.space?.id == space?.id {
            try? await Task.sleep(for: .milliseconds(250))
        }
        guard !Task.isCancelled else { return }
        await load(matching: text, in: space)
    }
}
