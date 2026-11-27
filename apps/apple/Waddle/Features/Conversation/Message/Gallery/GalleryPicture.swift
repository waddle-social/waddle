import SwiftUI
import WaddleKit

enum GalleryPicturePhase: View {
    case loading
    case still(Image)
    case animated(DecodedAnimatedImage, animates: Bool)
    case failed

    var isLoaded: Bool {
        switch self {
        case .still, .animated: true
        case .loading, .failed: false
        }
    }

    var body: some View {
        switch self {
        case .loading:
            ProgressView()
        case let .still(image):
            image.resizable().scaledToFit()
        case let .animated(image, animates):
            AnimatedImageRepresentable(image: image, animates: animates)
        case .failed:
            ContentUnavailableView("Couldn't load image", systemImage: "photo.badge.exclamationmark")
        }
    }
}

struct GalleryPicture<Content: View>: View {
    let file: SharedFile
    @ViewBuilder let content: (GalleryPicturePhase) -> Content

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase = GalleryPicturePhase.loading
    @State private var animationData: Data?

    var body: some View {
        content(phase)
            .task(id: reduceMotion) { await load(animates: !reduceMotion) }
    }

    private func load(animates: Bool) async {
        do {
            let data: Data
            if let animationData {
                data = animationData
            } else {
                guard case .loading = phase else { return }
                data = try await Self.fetch(file.url)
                try Task.checkCancellation()
                guard AnimatedImageMedia.isAnimated(data: data) else {
                    phase = Image(data: data).map(GalleryPicturePhase.still) ?? .failed
                    return
                }
                animationData = data
            }

            // Use the timeline's frame size and budget so the gallery keeps the same cadence.
            let image = await AnimatedImageDecoder.decodeInBackground(
                data, animated: animates, maxPixelSize: AnimatedImageDecoder.defaultMaxPixelSize
            )
            try Task.checkCancellation()
            phase = image.map { .animated($0, animates: animates) } ?? .failed
        } catch {
            if !Task.isCancelled { phase = .failed }
        }
    }

    private static func fetch(_ url: URL) async throws -> Data {
        if let cached = RemoteImageStore.shared.cached(url) { return cached }

        // Stills can exceed the timeline's fetch limit. Keep their original bytes on disk.
        let (download, response) = try await URLSession.shared.download(from: url)
        defer { try? FileManager.default.removeItem(at: download) }
        guard let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else {
            throw URLError(.badServerResponse)
        }
        return try Data(contentsOf: download, options: .mappedIfSafe)
    }
}
