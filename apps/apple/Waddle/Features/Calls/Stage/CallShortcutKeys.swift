import SwiftUI
import WaddleKit

/// The letter keys of a call's stage (`CallShortcut`), as buttons nobody
/// sees. A key with no modifier reaches a button only when nothing with
/// the keyboard wanted it, so a letter typed into a message is a letter.
struct CallShortcutKeys: View {
    let shortcuts: [CallShortcut]
    let perform: @MainActor (CallShortcut) -> Void
    @State private var repeats = CallShortcutRepeats()

    /// A key held down presses its button again and again: only the first counts.
    private func pressed(_ shortcut: CallShortcut) {
        let isFresh = repeats.isFreshPress(of: shortcut, at: ProcessInfo.processInfo.systemUptime)
        #if os(macOS)
        // A Mac's keyboard says which presses are repeats, and two quick
        // presses of F are two steps. Timing is for when it does not say.
        if let event = NSApp.currentEvent, event.type == .keyDown {
            if event.isARepeat { return }
        } else if !isFresh {
            return
        }
        #else
        // An iPad's keyboard does not say which presses are repeats.
        guard isFresh else { return }
        #endif
        perform(shortcut)
    }

    var body: some View {
        ZStack {
            ForEach(shortcuts, id: \.key) { shortcut in
                Button(shortcut.title) { pressed(shortcut) }
                    .keyboardShortcut(KeyEquivalent(shortcut.key), modifiers: [])
            }
        }
        .buttonStyle(.plain)
        .frame(width: 0, height: 0)
        .opacity(0)
        .allowsHitTesting(false)
        // The stage's own buttons say what they do; these would say it twice.
        .accessibilityHidden(true)
    }
}
