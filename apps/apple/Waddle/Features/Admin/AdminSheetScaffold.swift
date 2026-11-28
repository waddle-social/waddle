import SwiftUI

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
        var createTitle = "Create"
        var saveTitle = "Save"
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
                            .listRowBackground(Theme.Colors.surface)
                    }
                }
                if !isNew {
                    lists()
                    deleteSection
                }
            }
            .huddleForm()
            .tint(Theme.Colors.actionText)
            .disabled(isBusy)
            .navigationTitle(isNew ? copy.newTitle : copy.title)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                AdminSheetToolbar(
                    dismissTitle: isNew ? "Cancel" : "Close",
                    confirmTitle: isNew ? copy.createTitle : copy.saveTitle,
                    canDismiss: !isWaiting,
                    canConfirm: canSave && !isChanging,
                    isWorking: isBusy,
                    onDismiss: { dismiss() },
                    onConfirm: submit
                )
            }
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

    private var deleteSection: some View {
        Section {
            FormActionButton(title: copy.deleteTitle, symbol: "trash", isDestructive: true) {
                confirmsDelete = true
            }
            .disabled(isChanging)
            .opacity(isChanging || isBusy ? 0.5 : 1)
            .listRowBackground(Theme.Colors.surface)
            if let deleteError {
                ManagementErrorLabel(message: deleteError)
                    .listRowBackground(Theme.Colors.surface)
            }
        } footer: {
            FormSectionFooter(deleteWarning)
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
