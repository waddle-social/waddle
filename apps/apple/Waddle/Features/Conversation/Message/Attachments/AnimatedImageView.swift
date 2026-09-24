import SwiftUI

/// Plays GIF bytes (any other image shows still). Decodes off the main
/// actor; with Reduce Motion on it shows the first frame only. Sizes
/// itself by aspect fit inside the frame its parent gives it.
struct AnimatedImageView: View {
    let data: Data
    let maxPixelSize: CGFloat
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: Phase = .decoding

    private enum Phase {
        case decoding
        case decoded(DecodedAnimatedImage)
        case failed
    }

    private struct Request: Equatable {
        let data: Data
        let animates: Bool
        let maxPixelSize: CGFloat
    }

    init(data: Data, maxPixelSize: CGFloat = AnimatedImageDecoder.defaultMaxPixelSize) {
        self.data = data
        self.maxPixelSize = maxPixelSize
    }

    var body: some View {
        let request = Request(data: data, animates: !reduceMotion, maxPixelSize: maxPixelSize)
        content(animates: request.animates)
            .task(id: request) {
                let decoded = await AnimatedImageDecoder.decodeInBackground(
                    request.data,
                    animated: request.animates,
                    maxPixelSize: request.maxPixelSize
                )
                guard !Task.isCancelled else { return }
                phase = decoded.map(Phase.decoded) ?? .failed
            }
    }

    @ViewBuilder
    private func content(animates: Bool) -> some View {
        switch phase {
        case .decoding:
            Color.clear
        case let .decoded(image):
            AnimatedImageRepresentable(image: image, animates: animates)
        case .failed:
            Image(systemName: "photo.badge.exclamationmark")
                .font(.title2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}
