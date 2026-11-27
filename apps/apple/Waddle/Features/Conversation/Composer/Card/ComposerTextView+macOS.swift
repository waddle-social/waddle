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
    let text: String
    let draft: ComposerFieldEdits.Draft
    let edits: ComposerFieldEdits
    @Binding var selection: ComposerScalarRange?
    @Binding var isFocused: Bool
    let placeholder: String
    let onTextChange: (ComposerTextReplacement) -> Void
    let keys: ComposerKeyInput
    let selectionFormats: ComposerSelectionFormatCommands
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
        let coordinator = context.coordinator
        view.onFocusChange = { [weak coordinator] in coordinator?.focusChanged($0, focused: $1) }
        view.formattingBubble = coordinator.formattingBubble
        view.string = text
        coordinator.view = view

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
        let formatApplied = coordinator.parent.selectionFormats.formatRevision != selectionFormats.formatRevision
        coordinator.parent = self
        view.setAccessibilityLabel(placeholder)
        view.onPasteAttachments = onPasteAttachments
        view.onFormat = keys.format

        let draftChanged = coordinator.draft != draft || !view.string.utf16.elementsEqual(text.utf16)
        coordinator.isApplying = true
        let pending = edits.take(in: draft, from: view.string, to: text)
        if coordinator.draft == draft {
            for edit in pending { coordinator.apply(edit, to: view) }
        }
        if coordinator.draft != draft || !view.string.utf16.elementsEqual(text.utf16) {
            coordinator.draft = draft
            view.breakUndoCoalescing()
            coordinator.undoHistory.removeAllActions()
            view.string = text
            coordinator.pendingReplacement = nil
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
        coordinator.formattingBubble.update(
            suppression: selectionFormats.suppression, roomLinks: selectionFormats.roomLinks,
            draftChanged: draftChanged, formatApplied: formatApplied
        )

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

    static func dismantleNSView(_: NSScrollView, coordinator: Coordinator) {
        coordinator.formattingBubble.detach()
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
        var draft: ComposerFieldEdits.Draft
        weak var view: ComposerNSTextView?
        /// The offsets last passed between the field and the composer in
        /// either direction; a `selection` equal to it is an echo.
        var applied: ComposerScalarRange?
        /// Set while `updateNSView` writes into the view, whose change
        /// callbacks then describe the composer's own edit.
        var isApplying = false
        var pendingReplacement: ComposerScalarRange?
        /// The field's own undo history, cleared when the composer
        /// replaces the draft so no undo step points into text that is
        /// gone.
        let undoHistory = UndoManager()
        let measure = ComposerTextMeasure()
        lazy var formattingBubble = ComposerFormattingBubble { [weak self] action, view in
            guard let self, parent.text.utf16.elementsEqual(view.string.utf16) else { return }
            report(view)
            parent.selectionFormats.perform(action)
        }

        init(parent: ComposerTextView) {
            self.parent = parent
            draft = parent.draft
            super.init()
            for name in [Notification.Name.NSUndoManagerDidUndoChange, .NSUndoManagerDidRedoChange] {
                NotificationCenter.default.addObserver(self, selector: #selector(undoDidFinish(_:)), name: name, object: undoHistory)
            }
        }

        func undoManager(for _: NSTextView) -> UndoManager? {
            undoHistory
        }

        func apply(_ edit: ComposerFieldEdits.Edit, to view: ComposerNSTextView) {
            let changes = edit.replacement.nativeChanges(from: edit.original)
            let selection = ComposerUTF16Selection.nsRange(for: edit.selection, in: edit.replacement.text)
            guard !changes.isEmpty else {
                view.setSelectedRange(selection)
                return
            }
            // AppKit validates the ranges in document order. Replace from the
            // end so every range still points into the original draft.
            let ranges = changes.reversed().map { ComposerUTF16Selection.nsRange(for: $0.range, in: edit.original) }
            let before = view.selectedRange()
            view.breakUndoCoalescing()
            undoHistory.groupComposerEdit {
                undoHistory.registerComposerSelection(applyOnUndo: true) { [weak view] in view?.setSelectedRange(before) }
                if view.shouldChangeText(inRanges: ranges.map(NSValue.init(range:)),
                                         replacementStrings: changes.reversed().map(\.text)) {
                    for (range, change) in zip(ranges.reversed(), changes) {
                        view.replaceCharacters(in: range, with: change.text)
                    }
                    view.didChangeText()
                    view.setSelectedRange(selection)
                }
                undoHistory.registerComposerSelection(applyOnUndo: false) { [weak view] in view?.setSelectedRange(selection) }
            }
            view.breakUndoCoalescing()
        }

        @objc private func undoDidFinish(_: Notification) {
            guard !isApplying, let view else { return }
            pendingReplacement = nil
            parent.onTextChange(ComposerTextReplacement(view.string))
            report(view)
            formattingBubble.selectionChanged()
            formattingBubble.typed()
        }

        func textView(_ view: NSTextView, shouldChangeTextIn range: NSRange, replacementString _: String?) -> Bool {
            guard !isApplying, !undoHistory.isUndoing, !undoHistory.isRedoing else { return true }
            formattingBubble.typed()
            pendingReplacement = ComposerUTF16Selection.scalarRange(of: range, in: view.string)
            return true
        }

        func textDidChange(_ notification: Notification) {
            guard !isApplying, !undoHistory.isUndoing, !undoHistory.isRedoing,
                  let view = notification.object as? NSTextView else { return }
            formattingBubble.typed()
            formattingBubble.markedTextChanged(view.hasMarkedText())
            let replaced = pendingReplacement
            pendingReplacement = nil
            parent.onTextChange(ComposerTextReplacement(view.string, replacing: replaced))
            report(view)
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard !isApplying, !undoHistory.isUndoing, !undoHistory.isRedoing,
                  let view = notification.object as? NSTextView else { return }
            report(view)
            formattingBubble.selectionChanged()
        }

        func focusChanged(_ view: NSTextView, focused: Bool) {
            if focused { report(view) }
            formattingBubble.focusChanged(focused)
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
            let modifiers = NSApp.currentEvent?.modifierFlags.intersection(.deviceIndependentFlagsMask) ?? []
            let unmodified = modifiers.isDisjoint(with: [.shift, .control, .option, .command])
            if selector == #selector(NSResponder.insertNewline(_:)), unmodified {
                let selection = ComposerUTF16Selection.scalarRange(of: view.selectedRange(), in: view.string)
                return parent.keys.returnKey(view.string, selection, view.hasMarkedText(), .hardware)
            }
            // An input method composing text owns its keys.
            guard !view.hasMarkedText() else { return false }
            switch selector {
            case #selector(NSResponder.moveUp(_:)):
                return unmodified && parent.keys.suggestion(.up)
            case #selector(NSResponder.moveDown(_:)):
                return unmodified && parent.keys.suggestion(.down)
            case #selector(NSResponder.insertNewline(_:)):
                if !modifiers.isDisjoint(with: [.shift, .option]) {
                    view.insertNewlineIgnoringFieldEditor(nil)
                    return true
                }
                parent.keys.submit()
                return true
            case #selector(NSResponder.insertLineBreak(_:)):
                // A line separator would reach the message as U+2028.
                view.insertNewlineIgnoringFieldEditor(nil)
                return true
            case #selector(NSResponder.insertTab(_:)):
                if !parent.keys.suggestion(.tab) { view.window?.selectNextKeyView(nil) }
                return true
            case #selector(NSResponder.insertBacktab(_:)):
                view.window?.selectPreviousKeyView(nil)
                return true
            case #selector(NSResponder.cancelOperation(_:)):
                if parent.keys.suggestion(.escape) { return true }
                formattingBubble.escape()
                // Escape otherwise opens the text view's completion list;
                // pass it on so a sheet or panel can close instead.
                if !parent.keys.cancel() {
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
    var onFormat: ((ComposerFormat) -> Void)?
    weak var formattingBubble: ComposerFormattingBubble?
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

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        formattingBubble?.attach(to: self)
    }

    override func mouseDown(with event: NSEvent) {
        formattingBubble?.mouseTrackingBegan()
        super.mouseDown(with: event)
        formattingBubble?.mouseTrackingEnded()
    }

    override func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        formattingBubble?.markedTextChanged(true)
        super.setMarkedText(string, selectedRange: selectedRange, replacementRange: replacementRange)
        formattingBubble?.markedTextChanged(hasMarkedText())
    }

    override func unmarkText() {
        super.unmarkText()
        formattingBubble?.markedTextChanged(hasMarkedText())
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let modifiers = event.modifierFlags.intersection([.shift, .control, .option, .command])
        if window?.firstResponder === self, !hasMarkedText(), modifiers == [.command, .shift],
           let onFormat, let format = ComposerFormat.listShortcut(for: .ansi(event.keyCode)) {
            onFormat(format)
            return true
        }
        return super.performKeyEquivalent(with: event)
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
