import SwiftUI
import WaddleKit

struct MediaGalleryView: View {
    let gallery: MediaGallery
    let select: (MediaGallery.Image.ID) -> Void
    let close: () -> Void
    var standIn: Image?

    @AccessibilityFocusState private var focusedImage: MediaGallery.Image.ID?
    #if os(macOS)
    @FocusState private var keyboardFocused: Bool
    #endif
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif

    var body: some View {
        GeometryReader { geometry in
            let viewport = CGSize(
                width: min(geometry.size.width * (isCompact ? 1 : 0.92), 1_536),
                height: geometry.size.height * (isCompact ? 0.72 : 0.82)
            )
            ZStack {
                (isCompact ? Theme.Colors.bg : Theme.Material.mediaBackdrop).ignoresSafeArea()
                    .onTapGesture(perform: close)
                    .accessibilityHidden(true)
                GalleryZoomView(viewport: viewport, step: step, close: close, picture: picture) {
                    if !isCompact { caption }
                }
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
        #if os(macOS)
        .focusable()
        .focusEffectDisabled()
        .focused($keyboardFocused)
        .onAppear { keyboardFocused = true }
        .onKeyPress(.leftArrow) {
            step(.previous)
            return .handled
        }
        .onKeyPress(.rightArrow) {
            step(.next)
            return .handled
        }
        .onKeyPress(.escape) {
            close()
            return .handled
        }
        #endif
    }

    @ViewBuilder
    private var picture: some View {
        if let standIn {
            standIn.resizable().scaledToFit()
        } else {
            GalleryPicture(file: gallery.current.file)
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
        HStack(spacing: Theme.Spacing.s) {
            Text(gallery.current.name)
                .lineLimit(1)
                .truncationMode(.middle)
            if gallery.count > 1 {
                positionText.monospacedDigit().fixedSize()
            }
        }
        .huddleText(Theme.Typography.caption)
        .foregroundStyle(Theme.Colors.fgMuted)
        .frame(maxWidth: isCompact ? 320 : 672)
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
