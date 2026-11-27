import SwiftUI
import WaddleKit

struct MessageImagePreviewPresentation: ViewModifier {
    @Binding var file: SharedFile?
    let urls: Set<URL>

    func body(content: Content) -> some View {
        presenter(content)
            .onChange(of: urls) { _, _ in
                if previewFile == nil { file = nil }
            }
    }

    @ViewBuilder
    private func presenter(_ content: Content) -> some View {
        #if os(iOS)
        content.fullScreenCover(isPresented: isPresented) { preview }
        #elseif os(macOS)
        content.sheet(isPresented: isPresented) { preview }
        #endif
    }

    private var isPresented: Binding<Bool> {
        Binding(get: { previewFile != nil }, set: { if !$0 { file = nil } })
    }

    private var previewFile: SharedFile? {
        guard let file, urls.contains(file.url) else { return nil }
        return file
    }

    @ViewBuilder
    private var preview: some View {
        if let previewFile {
            MessageImagePreviewView(file: previewFile)
                .carryingCallDock()
        }
    }
}
