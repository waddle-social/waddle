import SwiftUI
import WaddleKit

/// Full-text search over a conversation's archive (XEP-0313 with a
/// full-text filter), debounced as the user types.
struct SearchSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    let conversation: ConversationID

    @State private var query = ""
    @State private var results: [TimelineItem] = []
    @State private var searchedQuery = ""
    @State private var isSearching = false

    init(conversation: ConversationID) {
        self.conversation = conversation
    }

    var body: some View {
        let header = ConversationHeaderText.make(for: conversation, session: session)
        NavigationStack {
            content(header: header)
                .navigationTitle("Search \(header.title)")
                #if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
                .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: Text("Search messages"))
                #else
                .searchable(text: $query, prompt: Text("Search messages"))
                #endif
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { dismiss() }
                    }
                }
                .task(id: query) {
                    await search(query)
                }
        }
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 520)
        #endif
    }

    @ViewBuilder
    private func content(header: ConversationHeaderText) -> some View {
        if trimmedQuery.isEmpty {
            ContentUnavailableView(
                header.searchPrompt,
                systemImage: "magnifyingglass",
                description: Text("Find messages by keyword.")
            )
        } else if results.isEmpty, isSearching || searchedQuery != trimmedQuery {
            ProgressView()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if results.isEmpty {
            ContentUnavailableView.search(text: searchedQuery)
        } else {
            List(results) { item in
                Button {
                    select(item)
                } label: {
                    ConversationSearchResultRow(item: item, query: searchedQuery)
                }
                .buttonStyle(.plain)
            }
            .listStyle(.plain)
        }
    }

    private var trimmedQuery: String {
        query.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func search(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            results = []
            searchedQuery = ""
            isSearching = false
            return
        }
        try? await Task.sleep(nanoseconds: 300_000_000)
        guard !Task.isCancelled else { return }
        isSearching = true
        let found = await session.search(trimmed, in: conversation)
        guard !Task.isCancelled else { return }
        results = found
        searchedQuery = trimmed
        isSearching = false
    }

    private func select(_ item: TimelineItem) {
        navigation.focus(item.id, in: conversation)
        dismiss()
    }
}

/// Author, date and the matching text with the query emphasized.
struct ConversationSearchResultRow: View {
    let item: TimelineItem
    let query: String

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(alignment: .firstTextBaseline) {
                Text(item.authorName)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.consistent(for: item.conversation.isRoom ? item.authorName : item.authorKey))
                    .lineLimit(1)
                Spacer(minLength: Theme.Spacing.s)
                Text(item.sentAt.formatted(date: .abbreviated, time: .shortened))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Text(highlighted)
                .font(.callout)
                .lineLimit(4)
        }
        .padding(.vertical, Theme.Spacing.xs)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var highlighted: AttributedString {
        var text = AttributedString(MessageAccessibilityText.content(of: item))
        if !query.isEmpty, let range = text.range(of: query, options: [.caseInsensitive, .diacriticInsensitive]) {
            text[range].inlinePresentationIntent = .stronglyEmphasized
            text[range].backgroundColor = Color.yellow.opacity(0.35)
        }
        return text
    }
}
