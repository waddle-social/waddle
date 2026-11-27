import SwiftUI
import WaddleKit

/// Zoom follows an image's identity, so changing images restores aspect fit.
struct GalleryZoomView<Picture: View, Caption: View>: View {
    let viewport: CGSize
    let step: (MediaGallery.Direction) -> Void
    let close: () -> Void
    let picture: Picture
    @ViewBuilder let caption: Caption

    @State private var transform = MediaGalleryViewport()
    @State private var pictureSize: CGSize = .zero
    @GestureState private var pinchScale: CGFloat = 1
    @GestureState private var dragTranslation: CGSize = .zero

    var body: some View {
        let displayed = displayedTransform
        let frame = displayed.imageFrame(pictureSize, in: viewport)
        let surface =
            picture
            .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.large))
            .onGeometryChange(for: CGSize.self, of: { $0.size }, action: { pictureSize = $0 })
            .scaleEffect(displayed.scale)
            .offset(displayed.offset)
            .frame(width: viewport.width, height: viewport.height)
            .clipped()
            .contentShape(Rectangle())
            .gesture(tap(in: frame))
            .simultaneousGesture(pinch)
            .simultaneousGesture(drag)
            .overlay(alignment: .top) {
                caption
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, (viewport.height + pictureSize.height) / 2 + Theme.Spacing.m)
                    .allowsHitTesting(false)
            }
        #if os(macOS)
        surface.background {
            GalleryScrollWheel { delta, precise in
                magnify(by: exp(delta * (precise ? 0.01 : 0.12)))
            }
        }
        #else
        surface
        #endif
    }

    private var displayedTransform: MediaGalleryViewport {
        var displayed = transform
        displayed.magnify(by: pinchScale, image: pictureSize, in: viewport)
        displayed.pan(by: dragTranslation, image: pictureSize, in: viewport)
        return displayed
    }

    private func tap(in imageFrame: CGRect) -> some Gesture {
        SpatialTapGesture(count: 2)
            .exclusively(before: SpatialTapGesture())
            .onEnded { result in
                switch result {
                case let .first(tap) where imageFrame.contains(tap.location):
                    transform.toggleZoom()
                case let .second(tap) where !imageFrame.contains(tap.location):
                    close()
                default:
                    break
                }
            }
    }

    private var pinch: some Gesture {
        MagnifyGesture()
            .updating($pinchScale) { value, scale, _ in scale = value.magnification }
            .onEnded { magnify(by: $0.magnification) }
    }

    private var drag: some Gesture {
        DragGesture()
            .updating($dragTranslation) { value, translation, _ in translation = value.translation }
            .onEnded { value in
                if transform.isZoomed {
                    transform.pan(by: value.translation, image: pictureSize, in: viewport)
                    return
                }
                #if os(iOS)
                if let direction = transform.swipe(value.translation) {
                    step(direction)
                }
                #endif
            }
    }

    private func magnify(by factor: CGFloat) {
        transform.magnify(by: factor, image: pictureSize, in: viewport)
    }
}
