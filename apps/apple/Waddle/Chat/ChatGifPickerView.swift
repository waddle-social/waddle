import SwiftUI

struct ChatGifPickerView: View {
    var onSelect: (String) -> Void
    @State private var searchText = ""
    @State private var results: [GiphyGif] = []
    @State private var isLoading = false
    @State private var searchTask: Task<Void, Never>?

    struct GiphyGif: Identifiable {
        let id: String
        let keywords: [String]
        let images: GiphyImages

        init(id: String, previewURL: String, originalURL: String, keywords: [String]) {
            self.id = id
            self.keywords = keywords
            self.images = GiphyImages(
                fixed_height_small: GiphyImage(url: previewURL, width: nil, height: nil),
                original: GiphyImage(url: originalURL, width: nil, height: nil)
            )
        }

        struct GiphyImages {
            let fixed_height_small: GiphyImage
            let original: GiphyImage
        }

        struct GiphyImage {
            let url: String
            let width: String?
            let height: String?
        }
    }

    private static let sampleGifs: [GiphyGif] = [
        GiphyGif(
            id: "celebrate",
            previewURL: "https://media1.giphy.com/media/111ebonMs90YLu/200w.gif",
            originalURL: "https://media1.giphy.com/media/111ebonMs90YLu/giphy.gif",
            keywords: ["celebrate", "party", "yay", "confetti"]
        ),
        GiphyGif(
            id: "thumbs-up",
            previewURL: "https://media1.giphy.com/media/XreQmk7ETCak0/200w.gif",
            originalURL: "https://media1.giphy.com/media/XreQmk7ETCak0/giphy.gif",
            keywords: ["thumbs", "up", "approve", "yes", "nice"]
        ),
        GiphyGif(
            id: "mind-blown",
            previewURL: "https://media1.giphy.com/media/OK27wINdQS5YQ/200w.gif",
            originalURL: "https://media1.giphy.com/media/OK27wINdQS5YQ/giphy.gif",
            keywords: ["wow", "mind", "blown", "surprised", "amazed"]
        ),
        GiphyGif(
            id: "laughing",
            previewURL: "https://media1.giphy.com/media/10JhviFuU2gWD6/200w.gif",
            originalURL: "https://media1.giphy.com/media/10JhviFuU2gWD6/giphy.gif",
            keywords: ["laugh", "funny", "lol", "haha"]
        ),
        GiphyGif(
            id: "wave",
            previewURL: "https://media1.giphy.com/media/ASd0Ukj0y3qMM/200w.gif",
            originalURL: "https://media1.giphy.com/media/ASd0Ukj0y3qMM/giphy.gif",
            keywords: ["hello", "wave", "hi", "welcome"]
        ),
        GiphyGif(
            id: "coffee",
            previewURL: "https://media1.giphy.com/media/oZEBLugoTthxS/200w.gif",
            originalURL: "https://media1.giphy.com/media/oZEBLugoTthxS/giphy.gif",
            keywords: ["coffee", "morning", "caffeine", "break"]
        ),
    ]

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textMuted)
                TextField("Search GIFs", text: $searchText)
                    .textFieldStyle(.plain)
                    .foregroundStyle(WaddleTheme.textPrimary)
                if !searchText.isEmpty {
                    Button {
                        searchText = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.caption)
                            .foregroundStyle(WaddleTheme.textMuted)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(WaddleTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 8))
            .padding(10)
            .onChange(of: searchText) { _, query in
                searchTask?.cancel()
                searchTask = Task {
                    try? await Task.sleep(nanoseconds: 300_000_000)
                    guard !Task.isCancelled else { return }
                    await fetchGifs(query: query)
                }
            }

            if isLoading {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if results.isEmpty {
                Text("No GIFs found")
                    .font(.caption)
                    .foregroundStyle(WaddleTheme.textMuted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 100), spacing: 4)], spacing: 4) {
                        ForEach(results) { gif in
                            Button {
                                onSelect(gif.images.original.url)
                            } label: {
                                AsyncImage(url: URL(string: gif.images.fixed_height_small.url)) { phase in
                                    switch phase {
                                    case .success(let image):
                                        image
                                            .resizable()
                                            .aspectRatio(contentMode: .fill)
                                            .frame(height: 80)
                                            .clipped()
                                    case .empty:
                                        WaddleTheme.surfaceRaised
                                            .frame(height: 80)
                                            .overlay { ProgressView() }
                                    default:
                                        WaddleTheme.surfaceRaised
                                            .frame(height: 80)
                                    }
                                }
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(8)
                }
            }
        }
        .frame(width: 360, height: 340)
        .background(WaddleTheme.sidebarBackground)
        .task {
            await fetchGifs(query: "")
        }
    }

    private func fetchGifs(query: String) async {
        isLoading = true
        defer { isLoading = false }

        let searchTerms = query
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)

        if searchTerms.isEmpty {
            results = Self.sampleGifs
            return
        }

        results = Self.sampleGifs.filter { gif in
            let haystack = ([gif.id] + gif.keywords).map { $0.lowercased() }
            return searchTerms.allSatisfy { term in
                haystack.contains { $0.contains(term) }
            }
        }
    }
}
