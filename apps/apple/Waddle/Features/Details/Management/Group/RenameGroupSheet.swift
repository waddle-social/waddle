import SwiftUI
import WaddleKit

/// Renames a group chat, or clears its name.
struct RenameGroupSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var editor: GroupRenameEditor
    @FocusState private var isNameFocused: Bool

    init(editor: GroupRenameEditor) {
        _editor = State(initialValue: editor)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $editor.name, prompt: Text("Group chat name"))
                        .labelsHidden()
                        .focused($isNameFocused)
                        .onSubmit(save)
                } footer: {
                    Text("Everyone in the group chat sees the new name. Leave it empty to remove the name.")
                }
                if let error = editor.saveError {
                    Section {
                        ManagementErrorLabel(message: error)
                    }
                }
            }
            .huddleForm()
            .disabled(editor.isSaving)
            .navigationTitle("Rename group chat")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
        }
        .interactiveDismissDisabled(editor.isSaving)
        .onAppear { isNameFocused = true }
        .managementSheetFrame(minHeight: 220)
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
