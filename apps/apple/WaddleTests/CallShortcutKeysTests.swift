#if os(macOS)
import AppKit
import SwiftUI
import WaddleKit
import XCTest

@testable import Waddle

/// The letter keys of a call, sent to a real window as the keyboard sends them.
@MainActor
final class CallShortcutKeysTests: XCTestCase {
    private var pressed: [CallShortcut] = []
    private var window: NSWindow!

    private func makeWindow(_ shortcuts: [CallShortcut], withField: Bool = false) -> NSTextView? {
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 320, height: 200), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let host = NSHostingView(rootView: Color.clear.background(CallShortcutKeys(shortcuts: shortcuts) { [unowned self] in pressed.append($0) }))
        host.frame = window.contentLayoutRect
        window.contentView = host
        var field: NSTextView?
        if withField {
            let text = NSTextView(frame: NSRect(x: 0, y: 0, width: 200, height: 40))
            host.addSubview(text)
            field = text
        }
        window.makeKeyAndOrderFront(nil)
        host.layoutSubtreeIfNeeded()
        return field
    }

    override func tearDown() async throws {
        window?.close()
        window = nil
    }

    private func press(_ key: String, modifiers: NSEvent.ModifierFlags = []) {
        let event = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber, context: nil, characters: key, charactersIgnoringModifiers: key,
            isARepeat: false, keyCode: 0
        )!
        window.sendEvent(event)
    }

    func testALetterWithNobodyTypingIsItsShortcut() {
        _ = makeWindow(CallShortcut.allCases)
        window.makeFirstResponder(nil)
        press("m")
        press("v")
        press("f")
        XCTAssertEqual(pressed, [.microphone, .camera, .stepUp])
    }

    func testALetterTypedIntoAFieldIsALetter() throws {
        let field = try XCTUnwrap(makeWindow(CallShortcut.allCases, withField: true))
        XCTAssertTrue(window.makeFirstResponder(field))
        press("m")
        XCTAssertEqual(pressed, [], "typing a message must not mute the call")
        XCTAssertEqual(field.string, "m")
    }

    func testAKeyTheStageHasNoUseForIsLeftAlone() {
        _ = makeWindow([.microphone])
        window.makeFirstResponder(nil)
        press("s")
        press("r")
        XCTAssertEqual(pressed, [])
    }

    func testAChordIsNotALetter() {
        _ = makeWindow(CallShortcut.allCases)
        window.makeFirstResponder(nil)
        press("m", modifiers: .command)
        XCTAssertEqual(pressed, [], "Command-M minimises the window")
    }
}
#endif
