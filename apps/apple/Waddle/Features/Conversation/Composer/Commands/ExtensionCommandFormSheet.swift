import SwiftUI
import WaddleKit

/// A XEP-0050 command stage as a XEP-0004 form, stage after stage until
/// the command completes or is cancelled.
struct ExtensionCommandFormSheet: View {
    let center: ComposerCommandCenter

    var body: some View {
        NavigationStack {
            if let stage = center.stage {
                ExtensionCommandFormContent(stage: stage, center: center)
            }
        }
        #if os(macOS)
        .frame(minWidth: 420, idealWidth: 480, minHeight: 360, idealHeight: 480)
        #endif
    }
}

private struct ExtensionCommandFormContent: View {
    @Environment(SessionCoordinator.self) private var session
    let stage: ExtensionCommandStage
    let center: ComposerCommandCenter

    var body: some View {
        Form {
            if let instructions = stage.result.form?.instructions, !instructions.isEmpty {
                Section {
                    Text(instructions)
                        .foregroundStyle(.secondary)
                }
            }
            if stage.isPending, stage.result.form?.blockedField != nil {
                Section {
                    Label(ExtensionCommandCopy.blockedFormMessage, systemImage: "lock")
                        .foregroundStyle(Color.orange)
                }
            }
            Section {
                ForEach(Array(stage.visibleFields.enumerated()), id: \.offset) { _, field in
                    ExtensionCommandFieldRow(field: field, isEditable: stage.isPending) { values in
                        center.setValues(values, for: field.variable)
                    }
                }
            }
            messages
        }
        .formStyle(.grouped)
        .disabled(center.isSubmitting)
        .navigationTitle(stage.result.form?.title ?? stage.command.name)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button(stage.isPending ? "Cancel" : "Done") {
                    center.dismiss(session: session)
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            ExtensionCommandActionBar(actions: stage.stepActions, isBusy: center.isSubmitting) { action in
                Task { await center.submit(action, session: session) }
            }
        }
    }

    @ViewBuilder
    private var messages: some View {
        if !stage.result.notes.isEmpty || center.formError != nil {
            Section {
                ForEach(Array(stage.result.notes.enumerated()), id: \.offset) { _, note in
                    Label(note.text, systemImage: symbol(for: note.type))
                        .foregroundStyle(color(for: note.type))
                }
                if let error = center.formError {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(Color.red)
                }
            }
        }
    }

    private func symbol(for type: ExtensionCommandNoteType) -> String {
        switch type {
        case .info: return "info.circle"
        case .warn: return "exclamationmark.triangle"
        case .error: return "exclamationmark.octagon"
        }
    }

    private func color(for type: ExtensionCommandNoteType) -> Color {
        switch type {
        case .info: return Color.primary
        case .warn: return Color.orange
        case .error: return Color.red
        }
    }
}
