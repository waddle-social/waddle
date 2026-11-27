import Foundation
import Testing
import WaddleKit

struct MediaGalleryViewportTests {
    private let viewport = CGSize(width: 300, height: 200)
    private let image = CGSize(width: 300, height: 60)

    @Test func doubleTapTogglesFitAndCenter() {
        var transform = MediaGalleryViewport()
        #expect(!transform.isZoomed)
        transform.toggleZoom()
        #expect(transform.isZoomed)
        #expect(transform.scale == 2)
        transform.pan(by: CGSize(width: 40, height: 10), image: image, in: viewport)
        #expect(transform.offset == CGSize(width: 40, height: 0))
        transform.toggleZoom()
        #expect(transform.scale == 1)
        #expect(transform.offset == .zero)
    }

    @Test func pinchingClampsZoomAndPanToTheScaledImage() {
        var transform = MediaGalleryViewport()
        transform.magnify(by: 10, image: image, in: viewport)
        #expect(transform.scale == 5)
        transform.pan(by: CGSize(width: 1_000, height: -1_000), image: image, in: viewport)
        #expect(transform.offset == CGSize(width: 600, height: -50))
        transform.magnify(by: 0.5, image: image, in: viewport)
        #expect(transform.scale == 2.5)
        #expect(transform.offset == CGSize(width: 225, height: 0))
        transform.magnify(by: 0.1, image: image, in: viewport)
        #expect(transform.scale == 1)
        #expect(transform.offset == .zero)
    }

    @Test func swipingStepsOnlyAtFitAndWithEnoughHorizontalTravel() {
        var transform = MediaGalleryViewport()
        #expect(transform.swipe(CGSize(width: -50, height: 20)) == .next)
        #expect(transform.swipe(CGSize(width: 50, height: 20)) == .previous)
        #expect(transform.swipe(CGSize(width: 20, height: 0)) == nil)
        #expect(transform.swipe(CGSize(width: 60, height: 80)) == nil)
        transform.toggleZoom()
        #expect(transform.swipe(CGSize(width: -50, height: 20)) == nil)
    }

    @Test func backdropHitTestingTracksTheVisibleImage() {
        var transform = MediaGalleryViewport()
        #expect(transform.imageFrame(image, in: viewport) == CGRect(x: 0, y: 70, width: 300, height: 60))
        #expect(!transform.imageFrame(image, in: viewport).contains(CGPoint(x: 50, y: 10)))

        transform.toggleZoom()
        transform.pan(by: CGSize(width: 100, height: 0), image: image, in: viewport)
        #expect(transform.imageFrame(image, in: viewport) == CGRect(x: -50, y: 40, width: 600, height: 120))
        #expect(transform.imageFrame(image, in: viewport).contains(CGPoint(x: 50, y: 55)))
    }

    @Test func missingImageLeavesTheWholeViewportAsBackdrop() {
        var transform = MediaGalleryViewport()
        transform.toggleZoom()
        let frame = transform.imageFrame(nil, in: viewport)
        #expect(frame.isNull)
        #expect(!frame.contains(CGPoint(x: viewport.width / 2, y: viewport.height / 2)))
    }
}
