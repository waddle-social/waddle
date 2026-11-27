import SwiftUI
import WaddleKit

struct GalleryDownloadButton: View {
    let file: SharedFile

    @State private var downloading = false
    @State private var exporting = false
    @State private var original: GalleryOriginal?
    @State private var failed = false

    var body: some View {
        Group {
            if downloading {
                ProgressView()
                    .controlSize(.small)
                    .frame(width: Theme.Size.minHitTarget, height: Theme.Size.minHitTarget)
                    .background(Theme.Material.media, in: Circle())
                    .accessibilityLabel(Text("Downloading original"))
            } else {
                HuddleIconButton("Download original", symbol: "arrow.down.to.line", side: Theme.Size.minHitTarget) {
                    downloading = true
                }
                .galleryControl()
                .disabled(exporting)
            }
        }
        .task(id: downloading) {
            if downloading { await download() }
        }
        .fileExporter(
            isPresented: $exporting, item: original, defaultFilename: original?.name,
            onCompletion: { result in
                finish()
                if case .failure = result { failed = true }
            },
            onCancellation: finish
        )
        .onDisappear(perform: finish)
        .alert("Something went wrong", isPresented: $failed) {
            Button("OK", role: .cancel) {}
        } message: {
            Text("Couldn't save the original image.")
        }
    }

    private func download() async {
        defer { downloading = false }
        do {
            let downloaded = try await GalleryOriginal.download(file)
            guard !Task.isCancelled else {
                downloaded.remove()
                return
            }
            original = downloaded
            exporting = true
        } catch {
            if !Task.isCancelled { failed = true }
        }
    }

    private func finish() {
        original?.remove()
        original = nil
    }
}
