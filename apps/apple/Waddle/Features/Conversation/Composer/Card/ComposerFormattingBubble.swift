#if os(macOS)
import AppKit
import SwiftUI
import WaddleKit

/// Adapts the editor's native selection and layout to one non-key panel.
@MainActor
final class ComposerFormattingBubble: NSObject {
    private static weak var visible: ComposerFormattingBubble?
    private weak var editor: NSTextView?
    private weak var window: NSWindow?
    private var panel: ComposerFormattingPanel?
    private var visibility = ComposerBubbleVisibility()
    private var roomLinks: [RoomLinkDraft] = []
    private var settleTask: Task<Void, Never>?
    private var renderQueued = false
    private var editorFrameInWindow: CGRect?
    private let perform: (ComposerSelectionFormatting.Action, NSTextView) -> Void

    init(perform: @escaping (ComposerSelectionFormatting.Action, NSTextView) -> Void) {
        self.perform = perform
    }

    func attach(to editor: NSTextView) {
        detach()
        guard let window = editor.window else { return }
        self.editor = editor
        self.window = window

        let center = NotificationCenter.default
        for event in [NSWindow.didMoveNotification, NSWindow.didResizeNotification, NSWindow.didChangeScreenNotification] {
            center.addObserver(self, selector: #selector(geometryChanged(_:)), name: event, object: window)
        }
        for event in [NSWindow.didResignKeyNotification, NSWindow.didBecomeKeyNotification] {
            center.addObserver(self, selector: #selector(windowKeyChanged(_:)), name: event, object: window)
        }
        center.addObserver(self, selector: #selector(windowWillClose(_:)), name: NSWindow.willCloseNotification, object: window)
        center.addObserver(self, selector: #selector(windowUpdated(_:)), name: NSWindow.didUpdateNotification, object: window)

        // A banner can move an ancestor without changing the scroll view's own frame.
        var ancestor: NSView? = editor
        while let view = ancestor {
            center.addObserver(self, selector: #selector(geometryChanged(_:)), name: NSView.frameDidChangeNotification, object: view)
            ancestor = view.superview
        }
        if let clip = editor.enclosingScrollView?.contentView {
            clip.postsBoundsChangedNotifications = true
            center.addObserver(self, selector: #selector(geometryChanged(_:)), name: NSView.boundsDidChangeNotification, object: clip)
        }

        send(.windowKeyChanged(window.isKeyWindow))
        send(.focusChanged(window.firstResponder === editor))
        selectionChanged()
    }

    func detach() {
        NotificationCenter.default.removeObserver(self)
        settleTask?.cancel()
        settleTask = nil
        removePanel()
        editor = nil
        window = nil
        editorFrameInWindow = nil
        let suppression = visibility.suppression
        visibility = ComposerBubbleVisibility()
        visibility.reduce(.suppressionChanged(suppression))
    }

    func update(
        suppression: ComposerBubbleVisibility.Suppression, roomLinks: [RoomLinkDraft], draftChanged: Bool, formatApplied: Bool
    ) {
        self.roomLinks = roomLinks
        send(.suppressionChanged(suppression))
        if formatApplied {
            send(.formatApplied(nativeSelection))
        } else {
            selectionChanged()
            if draftChanged { send(.typed) }
        }
    }

    func selectionChanged() {
        send(.markedTextChanged(editor?.hasMarkedText() ?? false))
        send(.selectionChanged(nativeSelection))
    }

    func mouseTrackingBegan() {
        send(.mouseDown)
        // NSTextView tracks the drag in its own event loop.
        removePanel()
    }

    func mouseTrackingEnded() {
        selectionChanged()
        send(.mouseUp)
    }

    func focusChanged(_ focused: Bool) { send(.focusChanged(focused)) }
    func markedTextChanged(_ marked: Bool) { send(.markedTextChanged(marked)) }
    func typed() { send(.typed) }
    func escape() { send(.escape) }

    private var nativeSelection: ComposerScalarRange? {
        guard let editor, editor.isEditable, editor.selectedRanges.count == 1 else { return nil }
        let range = editor.selectedRange()
        guard range.length > 0 else { return nil }
        return ComposerUTF16Selection.scalarRange(of: range, in: editor.string)
    }

    private var focusedEditor: NSTextView? {
        guard let editor, let window, editor.window === window,
              window.isKeyWindow, window.firstResponder === editor, !editor.hasMarkedText()
        else { return nil }
        return editor
    }

    private func send(_ event: ComposerBubbleVisibility.Event) {
        let pending = visibility.pendingSelection
        visibility.reduce(event)
        if pending != visibility.pendingSelection {
            settleTask?.cancel()
            settleTask = nil
            if visibility.pendingSelection != nil {
                settleTask = Task { [weak self] in
                    // TipTap waits 250 ms for selection changes to settle.
                    try? await Task.sleep(for: .milliseconds(250))
                    guard !Task.isCancelled else { return }
                    self?.send(.pauseElapsed)
                }
            }
        }
        requestRender()
    }

    @objc private func windowKeyChanged(_ notification: Notification) {
        selectionChanged()
        send(.windowKeyChanged(notification.name == NSWindow.didBecomeKeyNotification))
    }

    @objc private func windowWillClose(_: Notification) { detach() }

    @objc private func geometryChanged(_: Notification) { requestRender() }

    @objc private func windowUpdated(_: Notification) {
        guard visibility.visibleSelection != nil, let editor,
              editor.convert(editor.bounds, to: nil) != editorFrameInWindow
        else { return }
        requestRender()
    }

    private func requestRender() {
        guard editor != nil, !renderQueued else { return }
        renderQueued = true
        // Build and measure the hosting view on the next main-loop turn.
        // Its fitting size can re-enter layout during updateNSView.
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.renderQueued = false
            self.render()
        }
    }

    private func render() {
        guard let selection = visibility.visibleSelection, selection == nativeSelection,
              let editor = focusedEditor, let window,
              let formatting = ComposerSelectionFormatting.inDraft(editor.string, selection: selection, roomLinks: roomLinks)
        else {
            removePanel()
            return
        }
        editorFrameInWindow = editor.convert(editor.bounds, to: nil)
        // AppKit selects the screen that contains most of the window.
        guard let screen = window.screen,
              let anchor = visibleAnchor(for: editor.selectedRange(), in: editor, window: window)
        else {
            removePanel()
            return
        }

        if panel?.activeFormats != formatting.activeFormats { removePanel() }
        let bubble = panel ?? ComposerFormattingPanel(formatting: formatting) { [weak self] action in
            self?.take(action)
        }
        guard let frame = ComposerBubblePlacement.frame(
            for: bubble.frame.size, selection: anchor,
            windowContent: window.convertToScreen(window.contentLayoutRect), screenVisible: screen.visibleFrame,
            gap: Theme.Spacing.s
        ) else {
            if panel == nil { bubble.close() } else { removePanel() }
            return
        }
        bubble.appearance = editor.effectiveAppearance
        if bubble.frame != frame { bubble.setFrame(frame, display: panel != nil) }
        if panel == nil {
            Self.visible?.removePanel()
            Self.visible = self
            panel = bubble
            window.addChildWindow(bubble, ordered: .above)
            bubble.orderFront(nil)
        }
    }

    private func visibleAnchor(for selection: NSRange, in editor: NSTextView, window: NSWindow) -> CGRect? {
        let viewport = editor.visibleRect
        let visibleText = window.convertToScreen(editor.convert(viewport, to: nil))
        let first = editor.firstRect(forCharacterRange: selection, actualRange: nil)
        if first.intersects(visibleText) { return first.intersection(visibleText) }

        // A wrapped selection can start above the scrolled viewport.
        guard let layout = editor.layoutManager, let container = editor.textContainer else { return nil }
        let origin = editor.textContainerOrigin
        let glyphs = layout.glyphRange(forBoundingRect: viewport.offsetBy(dx: -origin.x, dy: -origin.y), in: container)
        let characters = layout.characterRange(forGlyphRange: glyphs, actualGlyphRange: nil)
        let visibleSelection = NSIntersectionRange(selection, characters)
        guard visibleSelection.length > 0 else { return nil }
        let anchor = editor.firstRect(forCharacterRange: visibleSelection, actualRange: nil)
        return anchor.intersects(visibleText) ? anchor.intersection(visibleText) : nil
    }

    private func take(_ action: ComposerSelectionFormatting.Action) {
        guard panel != nil, let selection = visibility.visibleSelection,
              selection == nativeSelection, let editor = focusedEditor
        else { return }
        if action == .link {
            send(.suppressionChanged(visibility.suppression.union(.linkPrompt)))
            removePanel()
        }
        perform(action, editor)
    }

    private func removePanel() {
        guard let panel else { return }
        self.panel = nil
        if Self.visible === self { Self.visible = nil }
        panel.parent?.removeChildWindow(panel)
        panel.close()
    }
}

/// The composer is inside SwiftUI split views. An AppKit child panel can
/// cross those clipping boundaries and receive a click without moving the
/// parent window's key status or the text view's first responder.
private final class ComposerFormattingPanel: NSPanel {
    let activeFormats: Set<ComposerFormat>

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }

    init(formatting: ComposerSelectionFormatting, perform: @escaping (ComposerSelectionFormatting.Action) -> Void) {
        activeFormats = formatting.activeFormats
        super.init(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        becomesKeyOnlyIfNeeded = true
        animationBehavior = .none
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        isReleasedWhenClosed = false

        let host = ComposerFormattingHostingView(rootView: ComposerFormattingBubbleContent(formatting: formatting, perform: perform))
        let size = host.fittingSize
        let toolbar = NSView(frame: CGRect(origin: .zero, size: size))
        toolbar.setAccessibilityElement(true)
        toolbar.setAccessibilityRole(.toolbar)
        toolbar.setAccessibilityLabel("Formatting")
        host.frame = toolbar.bounds
        host.autoresizingMask = [.width, .height]
        toolbar.addSubview(host)
        contentView = toolbar
        setContentSize(size)
    }
}

/// A click on a bubble button goes to SwiftUI even though the panel stays non-key.
private final class ComposerFormattingHostingView: NSHostingView<ComposerFormattingBubbleContent> {
    override func acceptsFirstMouse(for _: NSEvent?) -> Bool { true }
}
#endif
