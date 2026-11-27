import SwiftUI
import WaddleKit

/// The pane owns its selection so scrolling a message offscreen keeps the gallery open.
struct MediaGalleryPresentation: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Binding var selection: MediaGallery.Image.ID?
    let entries: [TimelineFeedEntry]
    #if os(macOS)
    @Environment(MacMediaGallery.self) private var window: MacMediaGallery?
    @State private var source = UUID()
    #endif

    func body(content: Content) -> some View {
        let gallery = selection.flatMap { selected in
            MediaGallery(items: entries.map(\.item), selected: selected) { session.remoteMediaDecision(for: $0) }
        }
        let content = content.onChange(of: selection != nil && gallery == nil) { _, invalid in
            // A removed or newly withheld image cannot reopen when it returns.
            if invalid { selection = nil }
        }
        #if os(macOS)
        content
            .onChange(of: gallery) { _, gallery in
                if let gallery {
                    window?.show(gallery, source: source, select: { selection = $0 }, close: { selection = nil })
                } else {
                    window?.dismiss(source: source)
                }
            }
            .onDisappear {
                window?.dismiss(source: source)
                selection = nil
            }
        #elseif os(iOS)
        let presented = Binding(get: { gallery != nil }, set: { if !$0 { selection = nil } })
        content.fullScreenCover(isPresented: presented) {
            if let gallery {
                MediaGalleryView(gallery: gallery, select: { selection = $0 }, close: { selection = nil })
                    .carryingCallDock()
                    .preferredColorScheme(.dark)
                    .presentationBackground(.clear)
            }
        }
        #endif
    }
}
