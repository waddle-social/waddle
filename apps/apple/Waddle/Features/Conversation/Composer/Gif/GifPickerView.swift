import SwiftUI
import WaddleKit

/// Searches GIPHY through the Waddle web app's proxy and hands back the
/// GIF picked; the caller sends its URL as the message body. Trending GIFs
/// show while the query is empty.
struct GifPickerView: View {
    let onPick: (GifSearchItem) -> Void
    let onCancel: () -> Void

    @State private var query: String
    /// The latest finished search; kept on screen while the next one runs.
    @State private var result: GifSearchResult?
    @State private var isSearching = false

    /// Typing pauses this long before a search starts.
    private static let debounceNanoseconds: UInt64 = 300_000_000
    private let client = GifSearchClient()

    init(initialQuery: String, onPick: @escaping (GifSearchItem) -> Void, onCancel: @escaping () -> Void) {
        _query = State(initialValue: initialQuery)
        self.onPick = onPick
        self.onCancel = onCancel
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                #if os(macOS)
                // `.searchable` puts its field in the window toolbar, which a
                // macOS sheet does not show reliably; search sits in the sheet.
                TextField("Search GIPHY", text: $query)
                    .textFieldStyle(.roundedBorder)
                    .padding(Theme.Spacing.m)
                #endif
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                GifPickerFooter(isSearching: isSearching && result != nil)
            }
            .navigationTitle("GIFs")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: Text("Search GIPHY"))
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close", systemImage: "xmark", action: onCancel)
                        .help("Close")
                }
            }
            .task(id: query) {
                await search(query)
            }
        }
        .presentationDetents([.medium, .large])
        #if os(macOS)
        .frame(minWidth: 440, minHeight: 480)
        #endif
    }

    @ViewBuilder
    private var content: some View {
        switch result {
        case nil:
            ProgressView()
        case let .results(items)?:
            if items.isEmpty {
                empty
            } else {
                GifPickerGrid(items: items, onPick: onPick)
            }
        case .notConfigured?:
            ContentUnavailableView(
                "GIF Search Unavailable",
                systemImage: "photo.on.rectangle.angled",
                description: Text("GIF search isn't set up on this server.")
            )
        case let .unavailable(message)?:
            ContentUnavailableView(
                "GIF Search Unavailable",
                systemImage: "exclamationmark.triangle",
                description: Text(message)
            )
        }
    }

    @ViewBuilder
    private var empty: some View {
        if trimmedQuery.isEmpty {
            ContentUnavailableView("No GIFs", systemImage: "photo.on.rectangle.angled")
        } else {
            ContentUnavailableView.search(text: trimmedQuery)
        }
    }

    private var trimmedQuery: String {
        query.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Debounced once results are showing; the first search runs at once.
    private func search(_ text: String) async {
        if result != nil {
            try? await Task.sleep(nanoseconds: Self.debounceNanoseconds)
            guard !Task.isCancelled else { return }
        }
        isSearching = true
        let found = await client.search(text)
        guard !Task.isCancelled else { return }
        result = found
        isSearching = false
    }
}
