#if os(macOS)
import SwiftUI
import WaddleKit

/// A window can show both a conversation and its thread. The pane owns the selection,
/// while the window provides the shared stage above both panes.
@MainActor
@Observable
final class MacMediaGallery {
    struct Presentation {
        let source: UUID
        let gallery: MediaGallery
        let select: (MediaGallery.Image.ID) -> Void
        let close: () -> Void
    }

    private(set) var presentation: Presentation?

    func show(
        _ gallery: MediaGallery, source: UUID, select: @escaping (MediaGallery.Image.ID) -> Void, close: @escaping () -> Void
    ) {
        presentation = Presentation(source: source, gallery: gallery, select: select, close: close)
    }

    func dismiss(source: UUID) {
        if presentation?.source == source { presentation = nil }
    }
}

struct MacMediaGalleryHost: ViewModifier {
    @State private var window = MacMediaGallery()

    func body(content: Content) -> some View {
        let presentation = window.presentation
        content
            .environment(window)
            .disabled(presentation != nil)
            .accessibilityHidden(presentation != nil)
            .overlay {
                if let presentation {
                    MediaGalleryView(gallery: presentation.gallery, select: presentation.select, close: presentation.close)
                        .carryingCallDock()
                }
            }
    }
}
#endif
