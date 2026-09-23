import SwiftUI
import WaddleKit

/// Creates a channel: name, optional description and a live address
/// preview.
struct NewChannelSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var summary = ""
    @State private var isCreating = false
    @State private var errorMessage: String?
    @FocusState private var isNameFocused: Bool

    init() {}

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $name, prompt: Text("e.g. design reviews"))
                        .focused($isNameFocused)
                        .onSubmit { create() }
                    TextField("Description", text: $summary, prompt: Text("What is it about? (optional)"), axis: .vertical)
                        .lineLimit(2...4)
                } footer: {
                    ChannelAddressPreview(name: name)
                }
                if let errorMessage {
                    Section {
                        Label(errorMessage, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .disabled(isCreating)
            .navigationTitle("New channel")
            .toolbar { sheetToolbar }
        }
        .interactiveDismissDisabled(isCreating)
        .onAppear { isNameFocused = true }
        .newChannelSheetFrame()
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
                    .disabled(slug == nil)
            }
        }
    }

    private var slug: String? {
        RoomLocalpart.make(from: name.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private func create() {
        guard slug != nil, !isCreating else { return }
        let trimmedSummary = summary.trimmingCharacters(in: .whitespacesAndNewlines)
        let channelName = name
        isCreating = true
        errorMessage = nil
        Task {
            do {
                let conversation = try await session.createChannel(
                    name: channelName,
                    summary: trimmedSummary.isEmpty ? nil : trimmedSummary
                )
                navigation.open(conversation)
                dismiss()
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't create the channel. Try again.")
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
                    .foregroundStyle(.primary)
            }
            .accessibilityElement(children: .combine)
        } else if name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            Text("The name becomes the channel address, like #design-reviews.")
        } else {
            Text("Use at least one letter or number.")
                .foregroundStyle(.red)
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
