import SwiftUI
import WaddleKit

/// Invites people to a group chat. An invitation only says it was sent:
/// whether they join is up to them, so the sheet closes without claiming
/// they were added.
struct AddPeopleSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var model: GroupInviteModel
    @FocusState private var isQueryFocused: Bool

    init(model: GroupInviteModel) {
        _model = State(initialValue: model)
    }

    var body: some View {
        NavigationStack {
            Form {
                recipientSection
                if !ConversationNameFilter.normalized(model.query).isEmpty {
                    RecipientResultsSection(
                        typedAddress: model.typedAddress,
                        search: model.search,
                        tokens: model.tokens,
                        onToggle: toggle
                    )
                }
                historySection
                if let error = model.sendError {
                    Section {
                        ManagementErrorLabel(message: error)
                    }
                }
            }
            .formStyle(.grouped)
            .disabled(model.isSending)
            .navigationTitle("Add people")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
            .task(id: model.query) { await model.runSearch(for: model.query, using: session) }
        }
        .interactiveDismissDisabled(model.isSending)
        .onAppear { isQueryFocused = true }
        .managementSheetFrame(minHeight: 460)
    }

    private var recipientSection: some View {
        Section {
            if !model.tokens.isEmpty {
                RecipientTokenStrip(tokens: model.tokens, onRemove: toggle)
            }
            // Verbatim: a localized key turns the address into a link,
            // which draws the prompt as ordinary text instead of a placeholder.
            TextField("Add", text: $model.query, prompt: Text(verbatim: "Name or address, like ana@example.com"))
                .labelsHidden()
                .addressInput()
                .focused($isQueryFocused)
                .onSubmit { model.addTypedAddress() }
        } footer: {
            if let member = model.typedMember {
                Text("\(member.description) is already in this group chat.")
            } else {
                Text("They get an invitation to join this group chat.")
            }
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
            Text("What they can see")
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { dismiss() }
                .disabled(model.isSending)
        }
        ToolbarItem(placement: .confirmationAction) {
            if model.isSending {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(Text("Sending invitations"))
            } else {
                Button("Invite", action: send)
                    .disabled(!model.canSend)
            }
        }
    }

    private func toggle(_ token: RecipientToken) {
        model.toggle(token)
        isQueryFocused = true
    }

    private func send() {
        Task {
            if await model.send(using: session) {
                dismiss()
            }
        }
    }
}

private extension View {
    func addressInput() -> some View {
        #if os(iOS)
        return self
            .textInputAutocapitalization(.never)
            .keyboardType(.emailAddress)
            .autocorrectionDisabled()
        #else
        return self.autocorrectionDisabled()
        #endif
    }
}
