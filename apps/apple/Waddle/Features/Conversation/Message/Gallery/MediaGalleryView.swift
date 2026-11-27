import SwiftUI
import WaddleKit

struct MediaGalleryView: View {
    let gallery: MediaGallery
    let select: (MediaGallery.Image.ID) -> Void
    let close: () -> Void
    var standIn: Image?

    @AccessibilityFocusState private var focusedImage: MediaGallery.Image.ID?
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif

    var body: some View {
        GeometryReader { geometry in
            let compactHeightFraction: CGFloat = 0.72
            // Regular-width limits mirror chat/src/components/ui/ImageLightbox.vue.
            let viewport = CGSize(
                width: min(geometry.size.width * (isCompact ? 1 : 0.92), 1_536),
                height: geometry.size.height * (isCompact ? compactHeightFraction : 0.82)
            )
            ZStack {
                (isCompact ? Theme.Colors.bg : Theme.Material.mediaBackdrop).ignoresSafeArea()
                    .onTapGesture(perform: close)
                    .accessibilityHidden(true)
                stage(in: viewport)
                    .id(gallery.current.id)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(Text(gallery.current.accessibilityName))
                    .accessibilityValue(positionText)
                    .accessibilityAddTraits(.isImage)
                    .accessibilityActions {
                        if gallery.neighbor(.previous) != nil { Button("Previous image") { step(.previous) } }
                        if gallery.neighbor(.next) != nil { Button("Next image") { step(.next) } }
                    }
                    .accessibilityFocused($focusedImage, equals: gallery.current.id)
                    .task(id: gallery.current.id) { focusedImage = gallery.current.id }

                if !isCompact, gallery.count > 1 {
                    arrows.frame(maxWidth: .infinity, alignment: .center)
                        .padding(.horizontal, Theme.Spacing.m)
                }
            }
            .overlay(alignment: .topTrailing) { toolbar.padding(Theme.Spacing.m) }
            .overlay(alignment: .bottom) {
                if isCompact {
                    ZStack {
                        caption.padding(.horizontal, Theme.Size.minHitTarget + Theme.Spacing.m)
                        if gallery.count > 1 { arrows }
                    }
                    .padding(Theme.Spacing.m)
                }
            }
        }
        .foregroundStyle(Theme.Material.mediaFg)
        .environment(\.colorScheme, .dark)
    }

    @ViewBuilder
    private func stage(in viewport: CGSize) -> some View {
        if let standIn {
            zoomView(.still(standIn), in: viewport)
        } else {
            GalleryPicture(file: gallery.current.file) { picture in
                zoomView(picture, in: viewport)
            }
        }
    }

    private func zoomView(_ picture: GalleryPicturePhase, in viewport: CGSize) -> some View {
        GalleryZoomView(viewport: viewport, step: step, close: close, picture: picture) {
            if !isCompact { caption }
        }
    }

    private var isCompact: Bool {
        #if os(iOS)
        horizontalSizeClass == .compact
        #else
        false
        #endif
    }

    private var toolbar: some View {
        HStack(spacing: Theme.Spacing.xs) {
            GalleryDownloadButton(file: gallery.current.file)
                .id(gallery.current.id)
            HuddleIconButton("Close gallery", symbol: "xmark", side: Theme.Size.minHitTarget, action: close)
                .galleryControl()
                .keyboardShortcut(.cancelAction)
        }
    }

    private var arrows: some View {
        HStack {
            arrow(.previous, label: "Previous image", symbol: "chevron.left", key: .leftArrow)
            Spacer()
            arrow(.next, label: "Next image", symbol: "chevron.right", key: .rightArrow)
        }
    }

    private func arrow(_ direction: MediaGallery.Direction, label: String, symbol: String, key: KeyEquivalent) -> some View {
        HuddleIconButton(label, symbol: symbol, side: Theme.Size.minHitTarget) { step(direction) }
            .galleryControl()
            .keyboardShortcut(key, modifiers: [])
            .disabled(gallery.neighbor(direction) == nil)
    }

    private var caption: some View {
        let compactCaptionWidth: CGFloat = 320
        let regularCaptionWidth: CGFloat = 672
        return HStack(spacing: Theme.Spacing.s) {
            Text(gallery.current.name)
                .lineLimit(1)
                .truncationMode(.middle)
            if gallery.count > 1 {
                positionText.monospacedDigit().fixedSize()
            }
        }
        .huddleText(Theme.Typography.caption)
        .foregroundStyle(Theme.Colors.fgMuted)
        .frame(maxWidth: isCompact ? compactCaptionWidth : regularCaptionWidth)
        .accessibilityHidden(true)
    }

    private var positionText: Text { Text("\(gallery.position) of \(gallery.count)") }

    private func step(_ direction: MediaGallery.Direction) {
        if let neighbor = gallery.neighbor(direction) { select(neighbor) }
    }
}

extension HuddleIconButton {
    func galleryControl() -> some View {
        background(Theme.Material.media, in: Circle())
    }
}
