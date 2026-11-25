import SwiftUI
import WaddleKit

/// The letter keys of a call's stage (`CallShortcut`), as buttons nobody
/// sees. A key with no modifier reaches a button only when nothing with
/// the keyboard wanted it, so a letter typed into a message is a letter.
struct CallShortcutKeys: View {
    let shortcuts: [CallShortcut]
    let perform: @MainActor (CallShortcut) -> Void

    var body: some View {
        ZStack {
            ForEach(shortcuts, id: \.key) { shortcut in
                Button(shortcut.title) { perform(shortcut) }
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
