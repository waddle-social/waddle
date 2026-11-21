import SwiftUI
import WaddleKit

/// What a sheet of the console is about: something new, or a row of the
/// list it was opened from.
enum AdminSheetSubject<Row: Identifiable>: Identifiable {
    case new
    case existing(Row)

    var id: Row.ID? { row?.id }

    var row: Row? {
        if case let .existing(row) = self { return row }
        return nil
    }
}

/// The frame every sheet of the console shares: a form with Cancel or
/// Close and Create or Save, what is shown while a save or a delete is on
/// its way and when it fails, and deleting behind a confirmation. A
/// change that fails keeps the sheet open and says why.
struct AdminSheetScaffold<Fields: View, Lists: View>: View {
    /// The words of one kind of sheet.
    struct Copy {
        let newTitle: String
        let title: String
        let saveFailure: String
        /// "Delete space", and the question the confirmation asks.
        let deleteTitle: String
        let deleteQuestion: String
        let deleteFailure: String
        let newMinHeight: CGFloat
        let minHeight: CGFloat
    }

    @Environment(\.dismiss) private var dismiss
    let isNew: Bool
    let copy: Copy
    /// What deleting takes with it, said under the button and asked again.
    let deleteWarning: String
    let canSave: Bool
    /// A change to one of the people listed is on its way to the server,
    /// so the sheet stays to show how it went.
    let isChanging: Bool
    /// Saves, then brings the list in step. Throwing keeps the sheet open.
    let save: () async throws -> Void
    let delete: () async throws -> Void
    /// The fields, given what Return in a field does.
    @ViewBuilder let fields: (_ submit: @escaping () -> Void) -> Fields
    /// Sections only something that exists has.
    @ViewBuilder let lists: () -> Lists
    @State private var isBusy = false
    @State private var saveError: String?
    @State private var deleteError: String?
    @State private var confirmsDelete = false

    var body: some View {
        NavigationStack {
            Form {
                fields(submit)
                if let saveError {
                    Section {
                        ManagementErrorLabel(message: saveError)
                    }
                }
                if !isNew {
                    lists()
                    deleteSection
                }
            }
            .formStyle(.grouped)
            .disabled(isBusy)
            .navigationTitle(isNew ? copy.newTitle : copy.title)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
        }
        .interactiveDismissDisabled(isWaiting)
        .confirmationDialog(copy.deleteQuestion, isPresented: $confirmsDelete, titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                run(delete, failure: copy.deleteFailure) { deleteError = $0 }
            }
            Button("Cancel", role: .cancel) {
                // Nothing to undo: the dialog closes itself.
            }
        } message: {
            Text(deleteWarning)
        }
        .managementSheetFrame(minHeight: isNew ? copy.newMinHeight : copy.minHeight)
    }

    private var isWaiting: Bool { isBusy || isChanging }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button(isNew ? "Cancel" : "Close") { dismiss() }
                .disabled(isWaiting)
        }
        ToolbarItem(placement: .confirmationAction) {
            if isBusy {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(Text("Working"))
            } else {
                Button(isNew ? "Create" : "Save", action: submit)
                    .disabled(!canSave || isChanging)
            }
        }
    }

    private var deleteSection: some View {
        Section {
            Button(copy.deleteTitle, role: .destructive) { confirmsDelete = true }
                .foregroundStyle(Theme.Colors.dangerText)
                .disabled(isChanging)
            if let deleteError {
                ManagementErrorLabel(message: deleteError)
            }
        } footer: {
            Text(deleteWarning)
        }
    }

    private func submit() {
        guard canSave else { return }
        run(save, failure: copy.saveFailure) { saveError = $0 }
    }

    /// Runs a save or a delete; the sheet closes when it worked.
    private func run(_ action: @escaping () async throws -> Void, failure: String, report: @escaping (String?) -> Void) {
        guard !isWaiting else { return }
        isBusy = true
        report(nil)
        Task {
            do {
                try await action()
                dismiss()
            } catch {
                report(ActionErrorCopy.message(for: error, fallback: failure))
            }
            isBusy = false
        }
    }
}

/// The name and description of a space or a room.
struct AdminNameFields: View {
    @Binding var name: String
    @Binding var description: String
    let namePrompt: String
    let descriptionPrompt: String
    /// It has a description and the field is blank: the server reads a
    /// blank one as "leave it".
    let keepsOldDescription: Bool
    let submit: () -> Void

    var body: some View {
        Section {
            TextField("Name", text: $name, prompt: Text(namePrompt))
                .labelsHidden()
                .onSubmit(submit)
        } header: {
            Text("Name")
        } footer: {
            if AdminName.isTooLong(name) {
                Text("A name can be up to \(AdminName.maxLength) characters.")
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        Section {
            TextField("Description", text: $description, prompt: Text(descriptionPrompt), axis: .vertical)
                .labelsHidden()
                .lineLimit(2...5)
        } header: {
            Text("Description")
        } footer: {
            if keepsOldDescription {
                Text("A description can be changed, but not removed yet.")
            }
        }
    }
}

/// A section of people in a sheet: what went wrong with the last change
/// to one of them, then the list, or that it is on its way.
struct AdminPeopleSection<Row: Identifiable & Sendable, RowView: View>: View {
    let title: String
    var footer: String?
    let list: AdminList<Row>?
    /// What the rows are, for "Couldn't refresh the …".
    let noun: String
    let emptyText: String
    var error: String?
    @ViewBuilder let row: (Row) -> RowView

    var body: some View {
        Section {
            if let error {
                ManagementErrorLabel(message: error)
            }
            if let list {
                AdminSheetList(list: list, noun: noun, emptyText: emptyText, row: row)
            } else {
                Text("Loading…")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        } header: {
            Text(title)
        } footer: {
            if let footer {
                Text(footer)
            }
        }
    }
}

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
                    Spacer(minLength: Theme.Spacing.xs)
                    Button("Try again") { Task { await list.reload() } }
                        .disabled(list.isLoading)
                }
            } else if list.rows.isEmpty {
                Text(list.hasLoaded ? emptyText : "Loading…")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            ForEach(Array(list.rows.enumerated()), id: \.element.id) { index, item in
                if index > 0 || list.failure != nil { Divider() }
                row(item)
            }
            if list.hasMore {
                Button(list.isLoadingMore ? "Loading…" : "Load more") {
                    Task { await list.loadMore() }
                }
                .disabled(list.isLoadingMore)
            }
            if list.moreFailure != nil {
                Text("Couldn't load more. Try again.")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
