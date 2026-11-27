#if os(iOS)
import SwiftUI
import UIKit
import WaddleKit

/// The iOS composer field: a `UITextView`, because SwiftUI's text field
/// pastes text only. A paste of a copied file, GIF or picture (the edit
/// menu's Paste, or Cmd-V) goes to `onPasteAttachments`; any other paste
/// is the text view's own. On a hardware keyboard, Return sends, Shift- or
/// Option-Return adds a line, Up/Down select a suggestion, Tab or Return accepts it,
/// and Escape closes the list or cancels an edit or reply, as on the Mac. The on-screen
/// keyboard's Return adds a line.
///
/// Grows from one line to `maxLines`, then scrolls.
struct ComposerTextView: UIViewRepresentable {
    let text: String
    @Binding var selection: ComposerScalarRange?
    @Binding var isFocused: Bool
    let placeholder: String
    let onTextChange: (ComposerTextReplacement) -> Void
    let onSubmit: () -> Void
    /// Returns true when an open suggestion list used the key.
    let onSuggestionKey: (ComposerHardwareKey) -> Bool
    /// Returns true when an edit or reply was cancelled.
    let onCancel: () -> Bool
    let onPasteAttachments: () -> Void

    static let maxLines: CGFloat = 8

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    func makeUIView(context: Context) -> ComposerUITextView {
        let view = ComposerUITextView()
        view.delegate = context.coordinator
        // The message style, scaled with Dynamic Type from the body size.
        let message = Theme.Typography.message
        view.font = UIFontMetrics(forTextStyle: .body).scaledFont(
            for: HuddleFonts.font(message.font, size: message.size, weight: message.weight) as UIFont
        )
        view.adjustsFontForContentSizeCategory = true
        view.textColor = UIColor(Theme.Colors.fg)
        view.tintColor = UIColor(Theme.Colors.actionText)
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.isScrollEnabled = false
        view.showsVerticalScrollIndicator = false
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.text = text
        return view
    }

    func updateUIView(_ view: ComposerUITextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        view.accessibilityLabel = placeholder
        view.keyHandler = coordinator.handle(_:)
        view.onPasteAttachments = onPasteAttachments

        coordinator.isApplying = true
        if view.text != text {
            // The composer replaced the draft (a send, a completion, a
            // formatting edit): the field's caret no longer describes it.
            view.text = text
            coordinator.pendingReplacement = nil
            coordinator.applied = nil
            if selection == nil {
                view.selectedRange = NSRange(location: (text as NSString).length, length: 0)
            }
        }
        if let selection, selection != coordinator.applied {
            coordinator.applied = selection
            let range = ComposerUTF16Selection.nsRange(for: selection, in: text)
            if view.selectedRange != range {
                view.selectedRange = range
            }
        }
        coordinator.isApplying = false

        // Focus moves after this update, so the text view's own focus
        // callbacks do not write state mid-update.
        if isFocused, !view.isFirstResponder {
            DispatchQueue.main.async {
                if view.window != nil { view.becomeFirstResponder() }
            }
        } else if !isFocused, view.isFirstResponder {
            DispatchQueue.main.async { view.resignFirstResponder() }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView view: ComposerUITextView, context _: Context) -> CGSize? {
        guard let width = proposal.width, width.isFinite, width > 0 else { return nil }
        let fitting = view.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        let lineHeight = (view.font ?? UIFont.preferredFont(forTextStyle: .body)).lineHeight
        let maxHeight = ceil(lineHeight * Self.maxLines)
        let scrolls = fitting.height > maxHeight + 0.5
        if view.isScrollEnabled != scrolls {
            DispatchQueue.main.async {
                view.isScrollEnabled = scrolls
                if scrolls { view.scrollRangeToVisible(view.selectedRange) }
            }
        }
        return CGSize(width: width, height: min(max(fitting.height, ceil(lineHeight)), maxHeight))
    }

    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: ComposerTextView
        /// The offsets last passed between the field and the composer in
        /// either direction; a `selection` equal to it is an echo.
        var applied: ComposerScalarRange?
        /// Set while `updateUIView` writes into the view, whose change
        /// callbacks then describe the composer's own edit.
        var isApplying = false
        var pendingReplacement: ComposerScalarRange?

        init(parent: ComposerTextView) {
            self.parent = parent
        }

        func textView(_ view: UITextView, shouldChangeTextIn range: NSRange, replacementText _: String) -> Bool {
            pendingReplacement = ComposerUTF16Selection.scalarRange(of: range, in: view.text)
            return true
        }

        func textViewDidChange(_ textView: UITextView) {
            guard !isApplying else { return }
            let replaced = pendingReplacement
            pendingReplacement = nil
            parent.onTextChange(ComposerTextReplacement(textView.text, replacing: replaced))
            report(textView)
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            guard !isApplying else { return }
            report(textView)
        }

        func textViewDidBeginEditing(_ textView: UITextView) {
            report(textView)
            if !parent.isFocused { parent.isFocused = true }
        }

        func textViewDidEndEditing(_: UITextView) {
            if parent.isFocused { parent.isFocused = false }
        }

        private func report(_ textView: UITextView) {
            guard let range = ComposerUTF16Selection.scalarRange(of: textView.selectedRange, in: textView.text),
                  range != applied
            else { return }
            applied = range
            parent.selection = range
        }

        /// Hardware keys; returns true when the key was used.
        func handle(_ key: ComposerHardwareKey) -> Bool {
            if parent.onSuggestionKey(key) { return true }
            switch key {
            case .returnKey:
                parent.onSubmit()
                return true
            case .escape:
                return parent.onCancel()
            case .tab, .up, .down:
                return false
            }
        }
    }
}

/// The text view behind `ComposerTextView`.
final class ComposerUITextView: UITextView {
    var onPasteAttachments: () -> Void = { /* The representable installs the handler on update. */ }
    var keyHandler: (ComposerHardwareKey) -> Bool = { _ in false }
    /// Presses whose begin the composer took, so their end is not passed
    /// on to a text view that never saw them begin.
    private var takenPresses: Set<UIPress> = []

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        // Offer Paste for a copied picture or file too; the check reads
        // only the pasteboard's types, which asks the person nothing.
        if action == #selector(paste(_:)), ComposerPasteboard.holdsAttachments {
            return true
        }
        return super.canPerformAction(action, withSender: sender)
    }

    override func paste(_ sender: Any?) {
        if ComposerPasteboard.holdsAttachments {
            onPasteAttachments()
        } else {
            super.paste(sender)
        }
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var passed = presses
        for press in presses {
            guard let key = Self.composerKey(for: press), markedTextRange == nil, keyHandler(key) else { continue }
            takenPresses.insert(press)
            passed.remove(press)
        }
        if !passed.isEmpty {
            super.pressesBegan(passed, with: event)
        }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let passed = release(presses)
        if !passed.isEmpty {
            super.pressesEnded(passed, with: event)
        }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let passed = release(presses)
        if !passed.isEmpty {
            super.pressesCancelled(passed, with: event)
        }
    }

    private func release(_ presses: Set<UIPress>) -> Set<UIPress> {
        let taken = presses.intersection(takenPresses)
        takenPresses.subtract(taken)
        return presses.subtracting(taken)
    }

    /// Composer hardware keys with no modifiers. Shift- and Option-Return
    /// stay with the text view, which inserts a line.
    private static func composerKey(for press: UIPress) -> ComposerHardwareKey? {
        guard let key = press.key else { return nil }
        let modifiers = key.modifierFlags.intersection([.shift, .control, .alternate, .command])
        guard modifiers.isEmpty else { return nil }
        switch key.keyCode {
        case .keyboardReturnOrEnter, .keypadEnter: return .returnKey
        case .keyboardTab: return .tab
        case .keyboardEscape: return .escape
        case .keyboardUpArrow: return .up
        case .keyboardDownArrow: return .down
        default: return nil
        }
    }
}
#endif
