import SwiftUI
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// Aspect-fit sizing for a platform image view, so it never asks for its
/// intrinsic (pixel) size and fits whatever frame its parent gives it.
enum AnimatedImageLayout {
    /// `natural` scaled to fit the proposal's specified dimensions; an
    /// unspecified proposal gets the natural size.
    static func fittedSize(_ natural: CGSize, in proposal: ProposedViewSize) -> CGSize {
        guard natural.width > 0, natural.height > 0 else { return .zero }
        var scales: [CGFloat] = []
        if let width = proposal.width, width.isFinite {
            scales.append(width / natural.width)
        }
        if let height = proposal.height, height.isFinite {
            scales.append(height / natural.height)
        }
        let scale = max(scales.min() ?? 1, 0)
        return CGSize(width: natural.width * scale, height: natural.height * scale)
    }
}

#if os(iOS)
/// A `UIImageView` showing a decoded image; a UIKit animated image plays by
/// itself. Touches pass through to the SwiftUI view around it.
struct AnimatedImageRepresentable: UIViewRepresentable {
    let image: DecodedAnimatedImage
    /// Reduce Motion is applied when decoding on iOS (one frame).
    let animates: Bool

    func makeUIView(context: Context) -> UIImageView {
        let view = UIImageView()
        view.contentMode = .scaleAspectFit
        view.clipsToBounds = true
        view.isUserInteractionEnabled = false
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.setContentHuggingPriority(.defaultLow, for: .vertical)
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .vertical)
        return view
    }

    func updateUIView(_ view: UIImageView, context: Context) {
        if view.image !== image.image {
            view.image = image.image
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UIImageView, context: Context) -> CGSize? {
        AnimatedImageLayout.fittedSize(image.size, in: proposal)
    }
}
#elseif os(macOS)
/// An `NSImageView`, which plays GIFs natively while `animates` is on.
/// Clicks pass through to the SwiftUI view around it.
struct AnimatedImageRepresentable: NSViewRepresentable {
    let image: DecodedAnimatedImage
    let animates: Bool

    func makeNSView(context: Context) -> PassthroughImageView {
        let view = PassthroughImageView()
        view.imageScaling = .scaleProportionallyUpOrDown
        view.imageFrameStyle = .none
        view.isEditable = false
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.setContentHuggingPriority(.defaultLow, for: .vertical)
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .vertical)
        return view
    }

    func updateNSView(_ view: PassthroughImageView, context: Context) {
        if view.image !== image.image {
            view.image = image.image
        }
        if view.animates != animates {
            view.animates = animates
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: PassthroughImageView, context: Context) -> CGSize? {
        AnimatedImageLayout.fittedSize(image.size, in: proposal)
    }
}

/// An image view that never takes the mouse, so a surrounding SwiftUI
/// button still receives the click.
final class PassthroughImageView: NSImageView {
    override func hitTest(_ point: NSPoint) -> NSView? {
        nil
    }
}
#endif
