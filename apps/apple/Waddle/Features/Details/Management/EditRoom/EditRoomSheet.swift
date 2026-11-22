import SwiftUI
import WaddleKit

/// Edits a room's name, description and who can pin messages. The details
/// header follows the directory, so a save only has to dismiss.
struct EditRoomSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var editor: RoomSettingsEditor
    /// Off for snapshots, which show a phase without a session.
    private let loadsOnAppear: Bool

    init(room: BareJID) {
        _editor = State(initialValue: RoomSettingsEditor(room: room))
        loadsOnAppear = true
    }

    init(editor: RoomSettingsEditor) {
        _editor = State(initialValue: editor)
        loadsOnAppear = false
    }

    var body: some View {
        NavigationStack {
            content
                .navigationTitle("Edit room")
                #if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
                #endif
                .toolbar { toolbar }
        }
        .interactiveDismissDisabled(editor.isSaving)
        .task {
            guard loadsOnAppear else { return }
            await editor.load(using: session)
        }
        .managementSheetFrame(minHeight: 400)
    }

    @ViewBuilder
    private var content: some View {
        switch editor.phase {
        case .loading:
            ManagementLoadingView(title: "Loading room settings…")
        case let .failed(message):
            ManagementLoadFailedView(title: "Couldn't load the room settings", message: message) {
                Task { await editor.load(using: session) }
            }
        case .loaded:
            EditRoomForm(editor: editor, onSubmit: save)
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { dismiss() }
                .disabled(editor.isSaving)
        }
        ToolbarItem(placement: .confirmationAction) {
            if editor.isSaving {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(Text("Saving"))
            } else {
                Button("Save", action: save)
                    .disabled(!editor.canSave)
            }
        }
    }

    private func save() {
        Task {
            if await editor.save(using: session) {
                dismiss()
            }
        }
    }
}

/// The loaded settings as a form.
struct EditRoomForm: View {
    @Bindable var editor: RoomSettingsEditor
    let onSubmit: () -> Void

    var body: some View {
        Form {
            Section {
                TextField("Name", text: $editor.name, prompt: Text("Room name"))
                    .labelsHidden()
                    .onSubmit(onSubmit)
            } header: {
                DetailsSectionHeader("Name")
            } footer: {
                nameFooter
            }
            Section {
                TextField("Description", text: $editor.summary, prompt: Text("What is it about? (optional)"), axis: .vertical)
                    .labelsHidden()
                    .lineLimit(2...5)
            } header: {
                DetailsSectionHeader("Description")
            }
            if editor.settings?.pinPermission != nil {
                Section {
                    Picker("Who can pin messages", selection: $editor.pinPermission) {
                        Text("Admins only").tag(PinPermission?.some(.adminsOnly))
                        Text("Anyone").tag(PinPermission?.some(.anyone))
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                } header: {
                    DetailsSectionHeader("Who can pin messages")
                }
            }
            if let error = editor.saveError {
                Section {
                    ManagementErrorLabel(message: error)
                }
            }
        }
        .detailsForm()
        .disabled(editor.isSaving)
    }

    @ViewBuilder
    private var nameFooter: some View {
        if editor.isNameMissing {
            Text("A room needs a name.")
                .foregroundStyle(Theme.Colors.dangerText)
        } else if let localpart = editor.room.localpart {
            Text("The address stays #\(localpart).")
        }
    }
}
