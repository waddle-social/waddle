import SwiftUI
import WaddleKit

/// A section of people in a sheet: what went wrong with the last change
/// to one of them, then the list, or that it is on its way, and a way to
/// add someone.
struct AdminPeopleSection<Row: Identifiable & Sendable, RowView: View>: View {
    let title: String
    var footer: String?
    let list: AdminList<Row>?
    /// What the rows are, for "Couldn't refresh the …".
    let noun: String
    let emptyText: String
    var error: String?
    /// "Add member", and what it does; nil for a list nothing is added to.
    var addTitle: String?
    var add: (() -> Void)?
    @ViewBuilder let row: (Row) -> RowView

    var body: some View {
        Section {
            if let error {
                ManagementErrorLabel(message: error)
                    .listRowBackground(Theme.Colors.surface)
            }
            if let list {
                AdminSheetList(list: list, noun: noun, emptyText: emptyText, row: row)
                    .listRowBackground(Theme.Colors.surface)
            } else {
                Text("Loading…")
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .listRowBackground(Theme.Colors.surface)
            }
            if let addTitle, let add {
                Button(action: add) {
                    Label(addTitle, systemImage: "plus")
                }
                .buttonStyle(.huddle(size: .small))
                .disabled(list == nil)
                .listRowBackground(Theme.Colors.surface)
            }
        } header: {
            FormSectionHeader(title)
        } footer: {
            if let footer {
                FormSectionFooter(footer)
            }
        }
    }
}
