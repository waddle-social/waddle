import SwiftUI
import WaddleKit

/// Adds people found by a name search to a group chat. Only people the
/// directory finds can be added: the server refuses other addresses
/// without telling us.
struct AddPeopleSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var model: GroupAddPeopleModel
    @FocusState private var isQueryFocused: Bool

    init(model: GroupAddPeopleModel) {
        _model = State(initialValue: model)
    }

    var body: some View {
        NavigationStack {
            Form {
                searchSection
                if !ConversationNameFilter.normalized(model.query).isEmpty {
                    RecipientResultsSection(
                        typedAddress: nil,
                        search: model.visibleSearch,
                        tokens: model.tokens,
                        emptyMessage: "No one found. People already in this group chat aren't listed.",
                        onToggle: toggle
                    )
                }
                historySection
                if let error = model.addError {
                    Section {
                        ManagementErrorLabel(message: error)
                    }
                }
            }
            .huddleForm()
            .disabled(model.isAdding)
            .navigationTitle("Add people")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
            .task(id: model.query) { await model.runSearch(for: model.query, using: session) }
        }
        .interactiveDismissDisabled(model.isAdding)
        .onAppear { isQueryFocused = true }
        .task {
            let session = self.session
            let room = model.room
            await model.excludeMembers { try await session.members(of: room) }
        }
        .managementSheetFrame(minHeight: 460)
    }

    private var searchSection: some View {
        Section {
            if !model.tokens.isEmpty {
                RecipientTokenStrip(tokens: model.tokens, onRemove: toggle)
            }
            TextField("Search people", text: $model.query, prompt: Text("Search people by name"))
                .labelsHidden()
                .autocorrectionDisabled()
                .focused($isQueryFocused)
        } footer: {
            Text("People you pick are added to this group chat.")
        }
    }

    private var historySection: some View {
        Section {
            Picker("What they can see", selection: $model.history) {
                Text("Messages from now on").tag(GroupDMHistoryAccess.fromJoin)
                Text("Earlier messages too").tag(GroupDMHistoryAccess.full)
            }
            .pickerStyle(.inline)
            .labelsHidden()
        } header: {
            FormSectionHeader("What they can see")
        } footer: {
            Text("Earlier messages are only shared if you can see all of them yourself. If not, they see messages from now on.")
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { dismiss() }
                .disabled(model.isAdding)
        }
        ToolbarItem(placement: .confirmationAction) {
            if model.isAdding {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(Text("Adding people"))
            } else {
                Button("Add", action: add)
                    .disabled(!model.canAdd)
            }
        }
    }

    private func toggle(_ token: RecipientToken) {
        model.toggle(token)
        isQueryFocused = true
    }

    private func add() {
        Task {
            if await model.add(using: session) {
                dismiss()
            }
        }
    }
}
