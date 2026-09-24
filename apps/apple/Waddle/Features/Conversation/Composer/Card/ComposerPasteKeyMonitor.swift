#if os(macOS)
import AppKit
import SwiftUI

/// Cmd-V in a focused text field goes to AppKit's field editor, which
/// takes text only, so SwiftUI's paste command never sees a copied file
/// or image. While the composer is focused in the key window, this takes
/// Cmd-V first when the pasteboard holds an attachment and hands it to
/// `onPaste`; every other Cmd-V reaches the field as usual.
struct ComposerPasteKeyMonitor: ViewModifier {
    let isFocused: Bool
    let onPaste: () -> Void

    @Environment(\.controlActiveState) private var activeState
    @State private var monitor = PasteKeyMonitor()

    func body(content: Content) -> some View {
        content
            .onAppear { sync() }
            .onChange(of: isFocused) { sync() }
            .onChange(of: activeState) { sync() }
            .onDisappear { monitor.stop() }
    }

    private func sync() {
        monitor.onPaste = onPaste
        if isFocused, activeState == .key {
            monitor.start()
        } else {
            monitor.stop()
        }
    }
}

/// Owns the local key-down monitor so it is removed exactly once.
final class PasteKeyMonitor {
    var onPaste: () -> Void = {}
    private var token: Any?

    func start() {
        guard token == nil else { return }
        token = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, Self.isPasteShortcut(event) else { return event }
            let pasted = MainActor.assumeIsolated { self.pasteAttachments() }
            return pasted ? nil : event
        }
    }

    /// Pastes only when the pasteboard holds something to attach.
    @MainActor
    private func pasteAttachments() -> Bool {
        guard ComposerPasteboard.holdsAttachments else { return false }
        onPaste()
        return true
    }

    func stop() {
        guard let token else { return }
        NSEvent.removeMonitor(token)
        self.token = nil
    }

    private static func isPasteShortcut(_ event: NSEvent) -> Bool {
        event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .command
            && event.charactersIgnoringModifiers?.lowercased() == "v"
    }
}
#endif
