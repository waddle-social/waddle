import SwiftUI

/// Where a `RemoteImageDataView` load is, like `AsyncImagePhase` but with
/// the bytes, which `AnimatedImageView` plays.
enum RemoteImageDataPhase {
    case empty
    case success(Data)
    case failure
}

/// Loads an image's bytes for `content`, restarting when `url` changes and
/// cancelling when the view goes away.
struct RemoteImageDataView<Content: View>: View {
    let url: URL
    private let content: (RemoteImageDataPhase) -> Content
    @State private var loaded: Loaded?

    /// A finished load; `data` is nil when it failed.
    private struct Loaded {
        let url: URL
        let data: Data?
    }

    init(url: URL, @ViewBuilder content: @escaping (RemoteImageDataPhase) -> Content) {
        self.url = url
        self.content = content
    }

    var body: some View {
        content(phase)
            .task(id: url) { await load(url) }
    }

    private var phase: RemoteImageDataPhase {
        if let loaded, loaded.url == url {
            return loaded.data.map(RemoteImageDataPhase.success) ?? .failure
        }
        if let cached = RemoteImageStore.shared.cached(url) {
            return .success(cached)
        }
        return .empty
    }

    private func load(_ url: URL) async {
        do {
            let data = try await RemoteImageStore.shared.load(url)
            guard !Task.isCancelled else { return }
            loaded = Loaded(url: url, data: data)
        } catch {
            guard !Task.isCancelled else { return }
            loaded = Loaded(url: url, data: nil)
        }
    }
}
