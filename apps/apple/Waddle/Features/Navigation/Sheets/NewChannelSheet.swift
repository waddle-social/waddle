import SwiftUI
import WaddleKit

/// Creates a room with a name and an optional description.
struct NewChannelSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var summary = ""
    @State private var isCreating = false
    @State private var errorMessage: String?
    @FocusState private var isNameFocused: Bool

    let space: Space?

    init(space: Space? = nil) {
        self.space = space
    }

    var body: some View {
        NavigationStack {
            styledForm
                .disabled(isCreating)
                .navigationTitle("New room")
                .toolbar { sheetToolbar }
        }
        .interactiveDismissDisabled(isCreating)
        .onAppear { isNameFocused = true }
        .newChannelSheetFrame()
    }

    @ViewBuilder
    private var styledForm: some View {
        if space == nil {
            form.formStyle(.grouped)
        } else {
            form.huddleForm()
        }
    }

    private var form: some View {
        Form {
            Section {
                styledField(
                    TextField("Name", text: $name, prompt: Text("e.g. design reviews"))
                        .focused($isNameFocused)
                        .onSubmit { create() }
                )
                styledField(
                    TextField("Description", text: $summary, prompt: Text("What is it about? (optional)"), axis: .vertical)
                        .lineLimit(2...4)
                )
            } footer: {
                if let space {
                    FormSectionFooter("This room will appear in \(space.name).")
                } else {
                    ChannelAddressPreview(name: name)
                }
            }
            if let errorMessage {
                Section {
                    Label(errorMessage, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(Theme.Colors.dangerText)
                }
            }
        }
    }

    @ViewBuilder
    private func styledField(_ field: some View) -> some View {
        if space != nil {
            field.huddleField()
        } else {
            field
        }
    }

    @ToolbarContentBuilder
    private var sheetToolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { dismiss() }
                .disabled(isCreating)
        }
        ToolbarItem(placement: .confirmationAction) {
            if isCreating {
                ProgressView()
                    .controlSize(.small)
            } else {
                Button("Create") { create() }
                    .disabled(!canCreate)
            }
        }
    }

    private var canCreate: Bool {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if space != nil {
            return !trimmed.isEmpty && !AdminName.isTooLong(trimmed)
        }
        return RoomLocalpart.make(from: trimmed) != nil
    }

    private func create() {
        guard canCreate, !isCreating else { return }
        let trimmedSummary = summary.trimmingCharacters(in: .whitespacesAndNewlines)
        let channelName = name
        isCreating = true
        errorMessage = nil
        Task {
            do {
                let description = trimmedSummary.isEmpty ? nil : trimmedSummary
                let conversation: ConversationID
                if let space {
                    conversation = try await session.createChannel(name: channelName, summary: description, in: space)
                } else {
                    conversation = try await session.createChannel(name: channelName, summary: description)
                }
                navigation.open(conversation)
                dismiss()
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't create the room. Try again.")
                isCreating = false
            }
        }
    }
}

/// "#slug" preview of the address a channel name produces.
struct ChannelAddressPreview: View {
    let name: String

    var body: some View {
        if let slug = RoomLocalpart.make(from: name.trimmingCharacters(in: .whitespacesAndNewlines)) {
            HStack(spacing: Theme.Spacing.xs) {
                Text("Address")
                Text("#\(slug)")
                    .font(.footnote.monospaced())
                    .foregroundStyle(Theme.Colors.fg)
            }
            .accessibilityElement(children: .combine)
        } else if name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            Text("The name becomes the room's address, like #design-reviews.")
        } else {
            Text("Use at least one letter or number.")
                .foregroundStyle(Theme.Colors.dangerText)
        }
    }
}

private extension View {
    func newChannelSheetFrame() -> some View {
        #if os(macOS)
        return self.frame(minWidth: 420, idealWidth: 460, minHeight: 280)
        #else
        return self
        #endif
    }
}
