#if os(macOS)
import AppKit
import XCTest

@testable import Waddle

/// Holding Space to talk, with real key events and a real window's keyboard focus.
@MainActor
final class CallStageKeyboardTests: XCTestCase {
    private var window: NSWindow!
    private var stage: NSView!
    private var field: NSTextView!
    private var keyboard: CallStageKeyboard!
    private var microphone: [Bool] = []

    override func setUp() async throws {
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 400, height: 300), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let content = NSView(frame: NSRect(x: 0, y: 0, width: 400, height: 300))
        window.contentView = content
        // The stage on top, the conversation's composer under it.
        stage = NSView(frame: NSRect(x: 0, y: 100, width: 400, height: 200))
        field = NSTextView(frame: NSRect(x: 0, y: 0, width: 400, height: 40))
        content.addSubview(stage)
        content.addSubview(field)
        window.makeKeyAndOrderFront(nil)
        keyboard = CallStageKeyboard()
        keyboard.hasMedia = true
        keyboard.setMicrophone = { [unowned self] on in
            microphone.append(on)
            keyboard.isMicrophoneOn = on
        }
        keyboard.start(watching: stage)
        window.makeFirstResponder(nil)
    }

    override func tearDown() async throws {
        keyboard.stop()
        window.close()
    }

    private func space(_ type: NSEvent.EventType, isRepeat: Bool = false, modifiers: NSEvent.ModifierFlags = []) -> NSEvent {
        NSEvent.keyEvent(
            with: type, location: .zero, modifierFlags: modifiers, timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber, context: nil, characters: " ", charactersIgnoringModifiers: " ",
            isARepeat: isRepeat, keyCode: 49
        )!
    }

    private func click(at point: NSPoint) -> NSEvent {
        NSEvent.mouseEvent(
            with: .leftMouseDown, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1
        )!
    }

    /// The microphone is set in a task, in order.
    private func settle() async {
        for _ in 0..<5 { await Task.yield() }
        try? await Task.sleep(for: .milliseconds(20))
    }

    func testHoldingSpaceOpensTheMicrophoneAndLettingGoClosesIt() async {
        XCTAssertNil(keyboard.handle(space(.keyDown)), "the key is the call's")
        XCTAssertNil(keyboard.handle(space(.keyDown, isRepeat: true)))
        await settle()
        XCTAssertEqual(microphone, [true])
        XCTAssertNil(keyboard.handle(space(.keyUp)))
        await settle()
        XCTAssertEqual(microphone, [true, false])
    }

    func testAQuickTapOpensThenCloses() async {
        _ = keyboard.handle(space(.keyDown))
        _ = keyboard.handle(space(.keyUp))
        await settle()
        XCTAssertEqual(microphone, [true, false], "never closed before it was opened")
    }

    func testSpaceTypedIntoAMessageIsASpace() async {
        XCTAssertTrue(window.makeFirstResponder(field))
        XCTAssertNotNil(keyboard.handle(space(.keyDown)))
        XCTAssertNotNil(keyboard.handle(space(.keyUp)))
        await settle()
        XCTAssertEqual(microphone, [])
    }

    func testAnOpenMicrophoneIsLeftOpen() async {
        keyboard.isMicrophoneOn = true
        XCTAssertNotNil(keyboard.handle(space(.keyDown)))
        XCTAssertNotNil(keyboard.handle(space(.keyUp)))
        await settle()
        XCTAssertEqual(microphone, [])
    }

    func testACallStillConnectingHasNoMicrophoneToOpen() async {
        keyboard.hasMedia = false
        XCTAssertNotNil(keyboard.handle(space(.keyDown)))
        await settle()
        XCTAssertEqual(microphone, [])
    }

    func testAChordWithSpaceIsNotOurs() async {
        XCTAssertNotNil(keyboard.handle(space(.keyDown, modifiers: .command)))
        await settle()
        XCTAssertEqual(microphone, [])
    }

    func testTheWindowLosingTheKeyboardClosesAHeldMicrophone() async {
        _ = keyboard.handle(space(.keyDown))
        await settle()
        NotificationCenter.default.post(name: NSWindow.didResignKeyNotification, object: window)
        await settle()
        XCTAssertEqual(microphone, [true, false])
    }

    func testTheStageGoingAwayClosesAHeldMicrophone() async {
        _ = keyboard.handle(space(.keyDown))
        keyboard.stop()
        await settle()
        XCTAssertEqual(microphone, [true, false])
    }

    func testAClickOnTheStageTakesTheKeyboardOutOfTheMessage() async {
        XCTAssertTrue(window.makeFirstResponder(field))
        XCTAssertNotNil(keyboard.handle(click(at: NSPoint(x: 200, y: 200))), "the click still goes where it was aimed")
        await settle()
        XCTAssertTrue(window.firstResponder === window)
    }

    func testAClickElsewhereLeavesTheMessageItsKeyboard() async {
        XCTAssertTrue(window.makeFirstResponder(field))
        _ = keyboard.handle(click(at: NSPoint(x: 200, y: 20)))
        await settle()
        XCTAssertTrue(window.firstResponder === field)
    }

    func testAStageGrownOverTheConversationTakesTheKeyboardFromItsHiddenComposer() {
        XCTAssertTrue(window.makeFirstResponder(field))
        keyboard.coversConversation = true
        XCTAssertTrue(window.firstResponder === window)
    }
}
#endif
