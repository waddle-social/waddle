#if os(iOS)
import SwiftUI
import UIKit

/// The iOS composer field: a `UITextView`, because SwiftUI's text field
/// pastes text only. A paste of a copied file, GIF or picture (the edit
/// menu's Paste, or Cmd-V) goes to `onPasteAttachments`; any other paste
/// is the text view's own. On a hardware keyboard, Return sends, Shift- or
/// Option-Return adds a line, Tab or Return accepts the first suggestion
/// and Escape cancels an edit or reply, as on the Mac. The on-screen
/// keyboard's Return adds a line.
///
/// Grows from one line to `maxLines`, then scrolls. Reports its
/// selection as Unicode scalar offsets, like the Mac field.
struct ComposerTextView: UIViewRepresentable {
    @Binding var text: String
    @Binding var selection: Range<Int>?
    @Binding var isFocused: Bool
    let placeholder: String
    let onSubmit: () -> Void
    /// Returns true when a suggestion was inserted.
    let onAcceptSuggestion: (ComposerSuggestionKey) -> Bool
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
        view.font = UIFont.preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.textColor = .label
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

    func sizeThatFits(_ proposal: ProposedViewSize, uiView view: ComposerUITextView, context: Context) -> CGSize? {
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
        var applied: Range<Int>?
        /// Set while `updateUIView` writes into the view, whose change
        /// callbacks then describe the composer's own edit.
        var isApplying = false

        init(parent: ComposerTextView) {
            self.parent = parent
        }

        func textViewDidChange(_ textView: UITextView) {
            guard !isApplying else { return }
            parent.text = textView.text
            report(textView)
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            guard !isApplying else { return }
            report(textView)
        }

        func textViewDidBeginEditing(_ textView: UITextView) {
            if !parent.isFocused { parent.isFocused = true }
        }

        func textViewDidEndEditing(_ textView: UITextView) {
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
            switch key {
            case .returnKey:
                if parent.onAcceptSuggestion(.returnKey) { return true }
                parent.onSubmit()
                return true
            case .tab:
                return parent.onAcceptSuggestion(.tab)
            case .escape:
                return parent.onCancel()
            }
        }
    }
}

/// Keys the composer takes from a hardware keyboard.
enum ComposerHardwareKey {
    case returnKey
    case tab
    case escape
}

/// The text view behind `ComposerTextView`.
final class ComposerUITextView: UITextView {
    var onPasteAttachments: () -> Void = {}
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

    /// Return, Tab and Escape with no modifiers. Shift- and Option-Return
    /// stay with the text view, which inserts a line.
    private static func composerKey(for press: UIPress) -> ComposerHardwareKey? {
        guard let key = press.key else { return nil }
        let modifiers = key.modifierFlags.intersection([.shift, .control, .alternate, .command])
        guard modifiers.isEmpty else { return nil }
        switch key.keyCode {
        case .keyboardReturnOrEnter, .keypadEnter: return .returnKey
        case .keyboardTab: return .tab
        case .keyboardEscape: return .escape
        default: return nil
        }
    }
}

/// Converts between a text view's UTF-16 selection and the Unicode scalar
/// offsets the WaddleKit formatting helpers use.
enum ComposerUTF16Selection {
    /// Scalar offsets of `range` in `text`; nil when `range` does not fall
    /// on scalar boundaries of `text`.
    static func scalarRange(of range: NSRange, in text: String) -> Range<Int>? {
        guard range.location != NSNotFound, let bounds = Range(range, in: text) else { return nil }
        let scalars = text.unicodeScalars
        guard let lower = bounds.lowerBound.samePosition(in: scalars),
              let upper = bounds.upperBound.samePosition(in: scalars)
        else { return nil }
        let start = scalars.distance(from: scalars.startIndex, to: lower)
        let end = scalars.distance(from: scalars.startIndex, to: upper)
        return start..<end
    }

    /// The UTF-16 range of scalar offsets `range`, clamped to `text`.
    static func nsRange(for range: Range<Int>, in text: String) -> NSRange {
        let scalars = text.unicodeScalars
        let count = scalars.count
        let lower = scalars.index(scalars.startIndex, offsetBy: min(max(range.lowerBound, 0), count))
        let upper = scalars.index(scalars.startIndex, offsetBy: min(max(range.upperBound, 0), count))
        return NSRange(lower..<upper, in: text)
    }
}
#endif
