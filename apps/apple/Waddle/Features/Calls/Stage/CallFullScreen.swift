import SwiftUI

extension View {
    /// Takes the window into full screen while a call is shown full screen
    /// (web `immersive`), and out again when the call steps down. On iPad
    /// the app already has the screen, so hiding the chrome is all there is.
    ///
    /// - Parameter exited: the window left full screen some other way, as
    ///   by its green button, while the call was still full screen.
    func callFullScreen(_ isOn: Bool, exited: @escaping @MainActor () -> Void) -> some View {
        #if os(macOS)
        modifier(CallFullScreen(isOn: isOn, exited: exited))
        #else
        self
        #endif
    }
}

#if os(macOS)
private struct CallFullScreen: ViewModifier {
    let isOn: Bool
    let exited: @MainActor () -> Void
    @State private var window: NSWindow?
    /// We put the window in full screen, so we take it out; a window the
    /// person had made full screen already stays that way.
    @State private var entered = false

    func body(content: Content) -> some View {
        content
            .background(WindowReader(window: $window))
            .onChange(of: isOn) { _, isOn in
                guard let window else { return }
                let isFullScreen = window.styleMask.contains(.fullScreen)
                if isOn, !isFullScreen {
                    entered = true
                    window.toggleFullScreen(nil)
                } else if !isOn {
                    if entered, isFullScreen { window.toggleFullScreen(nil) }
                    entered = false
                }
            }
            .onReceive(NotificationCenter.default.publisher(for: NSWindow.didExitFullScreenNotification)) { note in
                guard let window, note.object as? NSWindow === window else { return }
                entered = false
                if isOn { exited() }
            }
    }
}

private struct WindowReader: NSViewRepresentable {
    @Binding var window: NSWindow?

    func makeNSView(context: Context) -> NSView { NSView() }

    func updateNSView(_ view: NSView, context: Context) {
        // The view has no window until it is in one.
        DispatchQueue.main.async { if window !== view.window { window = view.window } }
    }
}
#endif
