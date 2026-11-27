#if os(macOS)
import AppKit
import SwiftUI

/// Observe wheel events in the picture viewport without taking mouse hits
/// from SwiftUI's double-click and drag gestures.
struct GalleryScrollWheel: NSViewRepresentable {
    let scroll: (CGFloat, Bool) -> Void

    func makeNSView(context: Context) -> WheelView { WheelView() }

    func updateNSView(_ view: WheelView, context: Context) {
        view.scroll = scroll
    }

    static func dismantleNSView(_ view: WheelView, coordinator: ()) {
        view.stopMonitoring()
    }

    final class WheelView: NSView {
        var scroll: ((CGFloat, Bool) -> Void)?
        private var monitor: Any?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            stopMonitoring()
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self] event in
                guard let self, let window, window.isKeyWindow, event.window === window,
                    bounds.contains(convert(event.locationInWindow, from: nil)), event.scrollingDeltaY != 0
                else { return event }
                if event.momentumPhase.isEmpty { scroll?(event.scrollingDeltaY, event.hasPreciseScrollingDeltas) }
                return nil
            }
        }

        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        func stopMonitoring() {
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
        }
    }
}
#endif
