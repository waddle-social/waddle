#if os(macOS)
import AppKit
import SwiftUI
import WaddleKit

/// The Mac composer field: an `NSTextView`. SwiftUI's text field reports
/// its text and its selection separately, and a render between the two
/// handed the field the previous keystroke's caret, so the next character
/// landed in front of the last one; a text view's edit and selection are
/// read together from the view itself. Return sends, Shift- or
/// Option-Return adds a line, Up/Down select a suggestion, Tab or Return accepts it,
/// and Escape closes the list or cancels an edit or reply, as on iOS. A paste of a copied
/// file, GIF or picture (the Edit menu's Paste, or Cmd-V) goes to
/// `onPasteAttachments`; any other paste is the text view's own.
///
/// Grows from one line to `maxLines`, then scrolls.
struct ComposerTextView: NSViewRepresentable {
    @Binding var text: String
    @Binding var selection: ComposerScalarRange?
    @Binding var isFocused: Bool
    let placeholder: String
    let onSubmit: () -> Void
    /// Returns true when an open suggestion list used the key.
    let onSuggestionKey: (ComposerHardwareKey) -> Bool
    /// Returns true when an edit or reply was cancelled.
    let onCancel: () -> Bool
    let onPasteAttachments: () -> Void

    static let maxLines: CGFloat = 8
    static let font: NSFont = {
        let message = Theme.Typography.message
        return HuddleFonts.font(message.font, size: message.size, weight: message.weight) as NSFont
    }()

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let view = ComposerNSTextView()
        view.delegate = context.coordinator
        view.font = Self.font
        view.textColor = NSColor(Theme.Colors.fg)
        view.insertionPointColor = NSColor(Theme.Colors.actionText)
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
        view.onFocusChange = context.coordinator.focusChanged(_:focused:)
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
        var applied: ComposerScalarRange?
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

        func focusChanged(_ view: NSTextView, focused: Bool) {
            if focused { report(view) }
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
            let modifiers = NSApp.currentEvent?.modifierFlags.intersection(.deviceIndependentFlagsMask) ?? []
            let unmodified = modifiers.isDisjoint(with: [.shift, .control, .option, .command])
            switch selector {
            case #selector(NSResponder.moveUp(_:)):
                return unmodified && parent.onSuggestionKey(.up)
            case #selector(NSResponder.moveDown(_:)):
                return unmodified && parent.onSuggestionKey(.down)
            case #selector(NSResponder.insertNewline(_:)):
                if !modifiers.isDisjoint(with: [.shift, .option]) {
                    view.insertNewlineIgnoringFieldEditor(nil)
                    return true
                }
                if unmodified, parent.onSuggestionKey(.returnKey) { return true }
                parent.onSubmit()
                return true
            case #selector(NSResponder.insertLineBreak(_:)):
                // A line separator would reach the message as U+2028.
                view.insertNewlineIgnoringFieldEditor(nil)
                return true
            case #selector(NSResponder.insertTab(_:)):
                if !parent.onSuggestionKey(.tab) { view.window?.selectNextKeyView(nil) }
                return true
            case #selector(NSResponder.insertBacktab(_:)):
                view.window?.selectPreviousKeyView(nil)
                return true
            case #selector(NSResponder.cancelOperation(_:)):
                // Escape otherwise opens the text view's completion list;
                // pass it on so a sheet or panel can close instead.
                if !parent.onSuggestionKey(.escape), !parent.onCancel() {
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
    var onFocusChange: ((NSTextView, Bool) -> Void)?
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
    /// conversation's drop target, which attaches it.
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
        if became { onFocusChange?(self, true) }
        return became
    }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { onFocusChange?(self, false) }
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
