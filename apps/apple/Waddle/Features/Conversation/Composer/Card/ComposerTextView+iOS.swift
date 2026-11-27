#if os(iOS)
import SwiftUI
import UIKit
import WaddleKit

/// The iOS composer field: a `UITextView`, because SwiftUI's text field
/// pastes text only. A paste of a copied file, GIF or picture (the edit
/// menu's Paste, or Cmd-V) goes to `onPasteAttachments`; any other paste
/// is the text view's own. On a hardware keyboard, Return sends, Shift- or
/// Option-Return adds a line, Up/Down select a suggestion, Tab or Return accepts it,
/// and Escape closes the list or cancels an edit or reply, as on the Mac.
/// Plain Return on either keyboard follows the composer’s block rules.
///
/// Grows from one line to `maxLines`, then scrolls.
struct ComposerTextView: UIViewRepresentable {
    let text: String
    let draft: ComposerFieldEdits.Draft
    let edits: ComposerFieldEdits
    @Binding var selection: ComposerScalarRange?
    @Binding var isFocused: Bool
    let placeholder: String
    let onTextChange: (ComposerTextReplacement) -> Void
    let keys: ComposerKeyInput
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
        view.keyHandler = coordinator.handle(_:in:)
        view.onFormat = keys.format
        view.onPasteAttachments = onPasteAttachments
        coordinator.observeUndo(in: view)

        coordinator.isApplying = true
        let pending = edits.take(in: draft, from: view.text, to: text)
        if coordinator.draft == draft {
            for edit in pending { coordinator.apply(edit, to: view) }
        }
        if coordinator.draft != draft || !view.text.utf16.elementsEqual(text.utf16) {
            coordinator.draft = draft
            view.undoManager?.removeAllActions()
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
        var draft: ComposerFieldEdits.Draft
        private weak var view: ComposerUITextView?
        private weak var observedUndoHistory: UndoManager?
        private var undoSelection: NSRange?
        private var isSettlingUndoSelection = false
        /// The offsets last passed between the field and the composer in
        /// either direction; a `selection` equal to it is an echo.
        var applied: ComposerScalarRange?
        /// Set while `updateUIView` writes into the view, whose change
        /// callbacks then describe the composer's own edit.
        var isApplying = false
        var pendingReplacement: ComposerScalarRange?

        init(parent: ComposerTextView) {
            self.parent = parent
            draft = parent.draft
        }

        func observeUndo(in view: ComposerUITextView) {
            self.view = view
            guard let undoHistory = view.undoManager, undoHistory !== observedUndoHistory else { return }
            for name in [Notification.Name.NSUndoManagerDidUndoChange, .NSUndoManagerDidRedoChange] {
                NotificationCenter.default.removeObserver(self, name: name, object: observedUndoHistory)
                NotificationCenter.default.addObserver(self, selector: #selector(undoDidFinish(_:)), name: name, object: undoHistory)
            }
            observedUndoHistory = undoHistory
        }

        func apply(_ edit: ComposerFieldEdits.Edit, to view: ComposerUITextView) {
            let changes = edit.replacement.nativeChanges(from: edit.original)
            let selection = ComposerUTF16Selection.nsRange(for: edit.selection, in: edit.replacement.text)
            guard !changes.isEmpty else {
                view.selectedRange = selection
                return
            }
            let before = view.selectedRange
            let undoHistory = view.undoManager
            let replace = {
                for change in changes {
                    let range = ComposerUTF16Selection.nsRange(for: change.range, in: edit.original)
                    guard let start = view.position(from: view.beginningOfDocument, offset: range.location),
                          let end = view.position(from: start, offset: range.length),
                          let replaced = view.textRange(from: start, to: end)
                    else { break }
                    view.replace(replaced, withText: change.text)
                }
                view.selectedRange = selection
            }
            if let undoHistory {
                undoHistory.groupComposerEdit {
                    undoHistory.registerComposerSelection(applyOnUndo: true) { [weak self] in self?.undoSelection = before }
                    replace()
                    undoHistory.registerComposerSelection(applyOnUndo: false) { [weak self] in self?.undoSelection = selection }
                }
            } else {
                replace()
            }
        }

        @objc private func undoDidFinish(_: Notification) {
            guard !isApplying, let view else { return }
            pendingReplacement = nil
            let text = view.text ?? ""
            let draft = self.draft
            let selection = undoSelection
            undoSelection = nil
            isSettlingUndoSelection = true
            parent.onTextChange(ComposerTextReplacement(text))
            // UIKit can set its own undo selection after posting the notification.
            DispatchQueue.main.async { [weak self, weak view] in
                guard let self else { return }
                isSettlingUndoSelection = false
                guard let view, self.draft == draft, view.text.utf16.elementsEqual(text.utf16) else { return }
                if let selection { view.selectedRange = selection }
                report(view)
            }
        }

        private func isReplayingUndo(_ view: UITextView) -> Bool {
            view.undoManager?.isUndoing == true || view.undoManager?.isRedoing == true
        }

        func textView(_ view: UITextView, shouldChangeTextIn range: NSRange, replacementText: String) -> Bool {
            guard !isApplying, !isReplayingUndo(view) else { return true }
            let selection = ComposerUTF16Selection.scalarRange(of: range, in: view.text)
            if replacementText == "\n", let field = view as? ComposerUITextView, field.isSoftwareReturn,
               parent.keys.returnKey(view.text, selection, view.markedTextRange != nil, .onScreen) {
                return false
            }
            pendingReplacement = selection
            return true
        }

        func textViewDidChange(_ textView: UITextView) {
            guard !isApplying, !isReplayingUndo(textView) else { return }
            let replaced = pendingReplacement
            pendingReplacement = nil
            parent.onTextChange(ComposerTextReplacement(textView.text, replacing: replaced))
            report(textView)
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            guard !isApplying, !isSettlingUndoSelection, !isReplayingUndo(textView) else { return }
            report(textView)
        }

        func textViewDidBeginEditing(_ textView: UITextView) {
            if let view = textView as? ComposerUITextView { observeUndo(in: view) }
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
        func handle(_ key: ComposerHardwareKey, in view: ComposerUITextView) -> Bool {
            if key == .returnKey {
                let selection = ComposerUTF16Selection.scalarRange(of: view.selectedRange, in: view.text)
                return parent.keys.returnKey(view.text, selection, view.markedTextRange != nil, .hardware)
            }
            return parent.keys.suggestion(key) || (key == .escape && parent.keys.cancel())
        }
    }
}

/// The text view behind `ComposerTextView`.
final class ComposerUITextView: UITextView {
    var onPasteAttachments: () -> Void = { /* The representable installs the handler on update. */ }
    var keyHandler: (ComposerHardwareKey, ComposerUITextView) -> Bool = { _, _ in false }
    var onFormat: (ComposerFormat) -> Void = { _ in /* The representable installs the handler on update. */ }
    private var nativeReturns: Set<UIPress> = []
    private var isPasting = false

    var isSoftwareReturn: Bool { nativeReturns.isEmpty && !isPasting }
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
            isPasting = true
            defer { isPasting = false }
            super.paste(sender)
        }
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var passed = presses
        for press in presses {
            if markedTextRange == nil, take(press) {
                takenPresses.insert(press)
                passed.remove(press)
            } else if press.key?.keyCode == .keyboardReturnOrEnter || press.key?.keyCode == .keypadEnter {
                // The delegate also sees native newlines, including Shift-Return
                // and any newline the input method lets through.
                nativeReturns.insert(press)
            }
        }
        if !passed.isEmpty {
            super.pressesBegan(passed, with: event)
        }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        defer { nativeReturns.subtract(presses) }
        let passed = release(presses)
        if !passed.isEmpty {
            super.pressesEnded(passed, with: event)
        }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        defer { nativeReturns.subtract(presses) }
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

    private func take(_ press: UIPress) -> Bool {
        guard let key = press.key else { return false }
        let modifiers = key.modifierFlags.intersection([.shift, .control, .alternate, .command])
        if modifiers == [.command, .shift], let format = ComposerFormat.listShortcut(for: .usb(key.keyCode.rawValue)) {
            onFormat(format)
            return true
        }
        guard let key = Self.composerKey(for: key, unmodified: modifiers.isEmpty) else { return false }
        return keyHandler(key, self)
    }

    /// Composer hardware keys with no modifiers. Shift- and Option-Return
    /// stay with the text view, which inserts a line.
    private static func composerKey(for key: UIKey, unmodified: Bool) -> ComposerHardwareKey? {
        guard unmodified else { return nil }
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
