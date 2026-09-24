import SwiftUI
import WaddleKit

/// One search hit: its small preview, playing; tap picks it.
struct GifPickerCell: View {
    let item: GifSearchItem
    let onPick: () -> Void

    static let height: CGFloat = 100

    var body: some View {
        Button(action: onPick) {
            RemoteImageDataView(url: item.previewURL) { phase in
                switch phase {
                case let .success(data):
                    AnimatedImageView(data: data, maxPixelSize: 240)
                case .failure:
                    Image(systemName: "photo.badge.exclamationmark")
                        .foregroundStyle(.secondary)
                case .empty:
                    ProgressView()
                        .controlSize(.small)
                }
            }
            .frame(maxWidth: .infinity)
            .frame(height: Self.height)
            .background(Color.secondary.opacity(0.12))
            .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(title))
        .accessibilityAddTraits(.isImage)
        .accessibilityHint(Text("Sends this GIF"))
        .help(title)
    }

    private var title: String {
        item.title.isEmpty ? "GIF" : item.title
    }
}
