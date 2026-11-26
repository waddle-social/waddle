#if os(macOS)
import AppKit
import WaddleKit
import XCTest

@testable import Waddle

/// A full-screen call's header and controls, against the real clock and a real key.
@MainActor
final class CallChromeFaderTests: XCTestCase {
    private func key() -> NSEvent {
        NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: 0, context: nil, characters: "x", charactersIgnoringModifiers: "x", isARepeat: false, keyCode: 7
        )!
    }

    private func rest() async throws {
        try await Task.sleep(for: .seconds(CallChromeFade.rest + 0.5))
    }

    func testTheyGoAfterARestAndAKeyBringsThemBack() async throws {
        let fader = CallChromeFader()
        fader.start()
        defer { fader.stop() }
        XCTAssertTrue(fader.isShown)
        try await rest()
        XCTAssertFalse(fader.isShown, "nothing stirred for a rest")
        NSApp.sendEvent(key())
        XCTAssertTrue(fader.isShown, "a key anywhere in the app brings them back")
        try await rest()
        XCTAssertFalse(fader.isShown)
    }

    func testSomethingOpenKeepsThem() async throws {
        let fader = CallChromeFader()
        fader.isHeld = true
        fader.start()
        defer { fader.stop() }
        try await rest()
        XCTAssertTrue(fader.isShown)
    }

    func testOutOfFullScreenTheyAlwaysShow() async throws {
        let fader = CallChromeFader()
        fader.start()
        try await rest()
        fader.stop()
        XCTAssertTrue(fader.isShown)
        fader.stir()
        try await rest()
        XCTAssertTrue(fader.isShown, "nothing fades once the call has stepped down")
    }
}
#endif
