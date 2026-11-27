import SwiftUI
import WaddleKit

struct GalleryPicture: View {
    let file: SharedFile

    var body: some View {
        if isGIF || canAnimate {
            RemoteImageDataView(url: file.url) { phase in
                switch phase {
                case let .success(data):
                    // Several frames fit the decoder's byte budget even for a square image.
                    AnimatedImageView(data: data, maxPixelSize: 1_024)
                case .empty: ProgressView()
                case .failure:
                    // Still PNG and WebP files can exceed the animation cache's byte limit.
                    if isGIF { failure } else { stillPicture }
                }
            }
        } else {
            stillPicture
        }
    }

    private var isGIF: Bool { GifMedia.isGIF(mediaType: file.mediaType, url: file.url) }

    private var canAnimate: Bool {
        ["image/png", "image/apng", "image/webp"].contains(file.mediaType?.lowercased() ?? "")
            || ["png", "apng", "webp"].contains(file.url.pathExtension.lowercased())
    }

    private var stillPicture: some View {
        AsyncImage(url: file.url) { phase in
            switch phase {
            case let .success(image): image.resizable().scaledToFit()
            case .failure: failure
            default: ProgressView()
            }
        }
    }

    private var failure: some View {
        ContentUnavailableView("Couldn't Load Image", systemImage: "photo.badge.exclamationmark")
    }
}
