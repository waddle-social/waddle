import SwiftUI
import WaddleKit

/// Starts a DM with one person or a group DM with several. People come
/// from XEP-0055 search or a typed address.
struct NewMessageSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var tokens: [RecipientToken] = []
    @State private var search = UserSearchState()
    @State private var groupName = ""
    @State private var isStarting = false
    @State private var startError: String?
    @FocusState private var isQueryFocused: Bool

    init() {}

    var body: some View {
        let typedAddress = RecipientInput.address(from: query)
        let recipients = RecipientInput.recipients(tokens: tokens, typedAddress: typedAddress)
        NavigationStack {
            Form {
                recipientSection
                if recipients.count > 1 {
                    Section("Group name") {
                        TextField("Group name", text: $groupName, prompt: Text(RecipientInput.suggestedGroupName(for: recipients)))
                    }
                }
                if !ConversationNameFilter.normalized(query).isEmpty {
                    RecipientResultsSection(
                        typedAddress: typedAddress,
                        search: search,
                        tokens: tokens,
                        onToggle: { toggle($0) }
                    )
                }
                if let startError {
                    Section {
                        Label(startError, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .disabled(isStarting)
            .navigationTitle("New message")
            .toolbar { sheetToolbar(recipients: recipients) }
            .task(id: query) { await runSearch(for: query) }
        }
        .interactiveDismissDisabled(isStarting)
        .onAppear { isQueryFocused = true }
        .newMessageSheetFrame()
    }

    private var recipientSection: some View {
        Section {
            if !tokens.isEmpty {
                RecipientTokenStrip(tokens: tokens) { token in
                    tokens = RecipientInput.toggled(token, in: tokens)
                }
            }
            TextField("To", text: $query, prompt: Text("Name or address, like ana@example.com"))
                .recipientAddressInput()
                .focused($isQueryFocused)
                .onSubmit { addTypedAddress() }
        } footer: {
            Text("Pick one person for a direct message, or several for a group chat.")
        }
    }

    @ToolbarContentBuilder
    private func sheetToolbar(recipients: [RecipientToken]) -> some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { dismiss() }
                .disabled(isStarting)
        }
        ToolbarItem(placement: .confirmationAction) {
            if isStarting {
                ProgressView()
                    .controlSize(.small)
            } else {
                Button(recipients.count > 1 ? "Create group" : "Start") {
                    start(with: recipients)
                }
                .disabled(recipients.isEmpty)
            }
        }
    }

    private func toggle(_ token: RecipientToken) {
        let wasPicked = tokens.contains { $0.jid == token.jid }
        tokens = RecipientInput.toggled(token, in: tokens)
        if !wasPicked {
            query = ""
        }
        isQueryFocused = true
    }

    /// Return in the field adds a typed address as a token.
    private func addTypedAddress() {
        guard let address = RecipientInput.address(from: query) else { return }
        if !tokens.contains(where: { $0.jid == address }) {
            tokens.append(RecipientToken(jid: address, name: nil))
        }
        query = ""
    }

    /// XEP-0055 search, debounced: `.task(id:)` cancels the previous run
    /// on every keystroke, so only a pause of 300 ms reaches the server.
    private func runSearch(for text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            search = UserSearchState()
            return
        }
        try? await Task.sleep(nanoseconds: 300_000_000)
        guard !Task.isCancelled else { return }
        search.isSearching = true
        do {
            let found = try await session.searchUsers(trimmed)
            guard !Task.isCancelled else { return }
            search = UserSearchState(
                results: RecipientInput.visibleResults(found, excluding: session.account.jid),
                isSearching: false,
                errorMessage: nil,
                hasSearched: true
            )
        } catch {
            guard !Task.isCancelled else { return }
            search = UserSearchState(
                results: [],
                isSearching: false,
                errorMessage: ActionErrorCopy.message(for: error, fallback: "Search isn't available right now."),
                hasSearched: true
            )
        }
    }

    private func start(with recipients: [RecipientToken]) {
        guard !recipients.isEmpty, !isStarting else { return }
        if recipients.count == 1 {
            let conversation = session.directConversation(with: recipients[0].jid)
            navigation.open(conversation)
            dismiss()
            return
        }
        let name = RecipientInput.groupName(typed: groupName, tokens: recipients)
        let members = recipients.map(\.jid)
        isStarting = true
        startError = nil
        Task {
            do {
                let conversation = try await session.createGroupDM(name: name, members: members)
                navigation.open(conversation)
                dismiss()
            } catch {
                startError = ActionErrorCopy.message(for: error, fallback: "Couldn't create the group chat. Try again.")
                isStarting = false
            }
        }
    }
}

/// The latest XEP-0055 search outcome.
struct UserSearchState: Equatable {
    var results: [UserSearchResult] = []
    var isSearching = false
    var errorMessage: String?
    /// A search for the current query has finished.
    var hasSearched = false
}

private extension View {
    func recipientAddressInput() -> some View {
        #if os(iOS)
        return self
            .textInputAutocapitalization(.never)
            .keyboardType(.emailAddress)
            .autocorrectionDisabled()
        #else
        return self.autocorrectionDisabled()
        #endif
    }

    func newMessageSheetFrame() -> some View {
        #if os(macOS)
        return self.frame(minWidth: 440, idealWidth: 480, minHeight: 420, idealHeight: 520)
        #else
        return self
        #endif
    }
}
