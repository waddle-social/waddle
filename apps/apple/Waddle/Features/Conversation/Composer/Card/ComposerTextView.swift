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

#elseif os(macOS)
import AppKit
import SwiftUI

/// The Mac composer field: an `NSTextView`. SwiftUI's text field reports
/// its text and its selection separately, and a render between the two
/// handed the field the previous keystroke's caret, so the next character
/// landed in front of the last one; a text view's edit and selection are
/// read together from the view itself. Return sends, Shift- or
/// Option-Return adds a line, Tab or Return accepts the first suggestion
/// and Escape cancels an edit or reply, as on iOS. A paste of a copied
/// file, GIF or picture (the Edit menu's Paste, or Cmd-V) goes to
/// `onPasteAttachments`; any other paste is the text view's own.
///
/// Grows from one line to `maxLines`, then scrolls. Reports its
/// selection as Unicode scalar offsets, like the iOS field.
struct ComposerTextView: NSViewRepresentable {
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
    static let font = NSFont.preferredFont(forTextStyle: .body)

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let view = ComposerNSTextView()
        view.delegate = context.coordinator
        view.font = Self.font
        view.textColor = .labelColor
        view.drawsBackground = false
        view.isRichText = false
        view.importsGraphics = false
        view.allowsUndo = true
        // Markdown and code go out as typed, without curled quotes or
        // dashes joined into one.
        view.isAutomaticQuoteSubstitutionEnabled = false
        view.isAutomaticDashSubstitutionEnabled = false
        view.isContinuousSpellCheckingEnabled = true
        view.textContainerInset = .zero
        view.minSize = .zero
        view.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: .greatestFiniteMagnitude)
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.onFocusChange = context.coordinator.focusChanged(_:)
        view.string = text

        let scrollView = NSScrollView()
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.documentView = view
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let view = scrollView.documentView as? ComposerNSTextView else { return }
        let coordinator = context.coordinator
        coordinator.parent = self
        view.setAccessibilityLabel(placeholder)
        view.onPasteAttachments = onPasteAttachments

        coordinator.isApplying = true
        if view.string != text {
            // The composer replaced the draft (a send, a completion, a
            // formatting edit): the field's caret and undo history no
            // longer describe it.
            view.string = text
            coordinator.undoHistory.removeAllActions()
            coordinator.applied = nil
            if selection == nil {
                view.setSelectedRange(NSRange(location: (text as NSString).length, length: 0))
            }
        }
        if let selection, selection != coordinator.applied {
            coordinator.applied = selection
            let range = ComposerUTF16Selection.nsRange(for: selection, in: text)
            if view.selectedRange() != range {
                view.setSelectedRange(range)
            }
        }
        coordinator.isApplying = false

        // Focus moves after this update, so the text view's own focus
        // callbacks do not write state mid-update.
        let isFirstResponder = view.window?.firstResponder === view
        if isFocused, !isFirstResponder {
            DispatchQueue.main.async {
                view.window?.makeFirstResponder(view)
            }
        } else if !isFocused, isFirstResponder {
            DispatchQueue.main.async {
                if view.window?.firstResponder === view { view.window?.makeFirstResponder(nil) }
            }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView scrollView: NSScrollView, context: Context) -> CGSize? {
        guard let width = proposal.width, width.isFinite, width > 0 else { return nil }
        let draft = (scrollView.documentView as? NSTextView)?.string ?? text
        let measure = context.coordinator.measure
        let lineHeight = ceil(measure.lineHeight(of: Self.font))
        let fitting = ceil(measure.height(of: draft, font: Self.font, width: width))
        return CGSize(width: width, height: min(max(fitting, lineHeight), lineHeight * Self.maxLines))
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: ComposerTextView
        /// The offsets last passed between the field and the composer in
        /// either direction; a `selection` equal to it is an echo.
        var applied: Range<Int>?
        /// Set while `updateNSView` writes into the view, whose change
        /// callbacks then describe the composer's own edit.
        var isApplying = false
        /// The field's own undo history, cleared when the composer
        /// replaces the draft so no undo step points into text that is
        /// gone.
        let undoHistory = UndoManager()
        let measure = ComposerTextMeasure()

        init(parent: ComposerTextView) {
            self.parent = parent
        }

        func undoManager(for _: NSTextView) -> UndoManager? {
            undoHistory
        }

        func textDidChange(_ notification: Notification) {
            guard !isApplying, let view = notification.object as? NSTextView else { return }
            parent.text = view.string
            report(view)
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard !isApplying, let view = notification.object as? NSTextView else { return }
            report(view)
        }

        func focusChanged(_ focused: Bool) {
            if parent.isFocused != focused { parent.isFocused = focused }
        }

        private func report(_ view: NSTextView) {
            guard let range = ComposerUTF16Selection.scalarRange(of: view.selectedRange(), in: view.string),
                  range != applied
            else { return }
            applied = range
            parent.selection = range
        }

        /// Keys, as the text system's commands; returns true when the
        /// composer took the command.
        func textView(_ view: NSTextView, doCommandBy selector: Selector) -> Bool {
            // An input method composing text owns its keys.
            guard !view.hasMarkedText() else { return false }
            switch selector {
            case #selector(NSResponder.insertNewline(_:)):
                let modifiers = NSApp.currentEvent?.modifierFlags.intersection(.deviceIndependentFlagsMask) ?? []
                if !modifiers.isDisjoint(with: [.shift, .option]) {
                    view.insertNewlineIgnoringFieldEditor(nil)
                    return true
                }
                if parent.onAcceptSuggestion(.returnKey) { return true }
                parent.onSubmit()
                return true
            case #selector(NSResponder.insertLineBreak(_:)):
                // A line separator would reach the message as U+2028.
                view.insertNewlineIgnoringFieldEditor(nil)
                return true
            case #selector(NSResponder.insertTab(_:)):
                if !parent.onAcceptSuggestion(.tab) { view.window?.selectNextKeyView(nil) }
                return true
            case #selector(NSResponder.insertBacktab(_:)):
                view.window?.selectPreviousKeyView(nil)
                return true
            case #selector(NSResponder.cancelOperation(_:)):
                // Escape otherwise opens the text view's completion list;
                // pass it on so a sheet or panel can close instead.
                if !parent.onCancel() {
                    view.nextResponder?.tryToPerform(selector, with: nil)
                }
                return true
            default:
                return false
            }
        }
    }
}

/// The text view behind the Mac `ComposerTextView`, on TextKit 1 so
/// `ComposerTextMeasure` sizes the field with the same layout.
final class ComposerNSTextView: NSTextView {
    var onPasteAttachments: (() -> Void)?
    var onFocusChange: ((Bool) -> Void)?
    /// A text view does not retain the storage behind a container it is
    /// given.
    private let storage: NSTextStorage

    init() {
        let storage = NSTextStorage()
        let layout = NSLayoutManager()
        storage.addLayoutManager(layout)
        let container = NSTextContainer(size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = true
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        self.storage = storage
        super.init(frame: .zero, textContainer: container)
    }

    required init?(coder _: NSCoder) {
        fatalError("ComposerNSTextView is created in code")
    }

    /// Text only: a dropped file or picture goes past the field to the
    /// composer's drop target, which attaches it.
    override var acceptableDragTypes: [NSPasteboard.PasteboardType] {
        [.string]
    }

    override func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        // Offer Paste for a copied picture or file too; the check reads
        // only the pasteboard's types.
        if item.action == #selector(paste(_:)), ComposerPasteboard.holdsAttachments {
            return true
        }
        return super.validateUserInterfaceItem(item)
    }

    override func paste(_ sender: Any?) {
        if ComposerPasteboard.holdsAttachments {
            onPasteAttachments?()
        } else {
            super.paste(sender)
        }
    }

    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { onFocusChange?(true) }
        return became
    }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { onFocusChange?(false) }
        return resigned
    }
}

/// Lays out a draft off screen, as `ComposerNSTextView` would, to size the
/// field for a width it has not been given yet.
@MainActor
final class ComposerTextMeasure {
    private let storage = NSTextStorage()
    private let layout = NSLayoutManager()
    private let container = NSTextContainer(size: .zero)

    init() {
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
    }

    /// Height of `text` wrapped to `width`, counting the empty line after
    /// a trailing newline, where the caret sits.
    func height(of text: String, font: NSFont, width: CGFloat) -> CGFloat {
        container.size = NSSize(width: width, height: .greatestFiniteMagnitude)
        storage.setAttributedString(NSAttributedString(string: text, attributes: [.font: font]))
        layout.ensureLayout(for: container)
        return layout.usedRect(for: container).height
    }

    func lineHeight(of font: NSFont) -> CGFloat {
        layout.defaultLineHeight(for: font)
    }
}
#endif
