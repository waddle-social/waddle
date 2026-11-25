import Observation
import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// What a call's stage redraws, and what it writes, when nothing it shows
/// has changed. On a Mac every camera is drawn on the main thread (#2625),
/// so work repeated each time someone speaks is paid for in dropped frames.
@MainActor
final class CallStageRedrawTests: XCTestCase {
    private static let you = CallStageTile(
        identity: "ada@waddle.test/mac", source: .camera, label: "You", isSelf: true, hasVideo: true
    )

    private static func tile(_ name: String, isSpeaking: Bool = false, hasVideo: Bool = true) -> CallStageTile {
        CallStageTile(
            identity: "\(name)@waddle.test/web", source: .camera, label: name, isSpeaking: isSpeaking, hasVideo: hasVideo
        )
    }

    /// The tiles on stage, changed from outside the way a media session changes them.
    @Observable
    final class Roster {
        var tiles: [CallStageTile]
        init(_ tiles: [CallStageTile]) { self.tiles = tiles }
    }

    /// Counts how often each tile's picture is asked for.
    final class Cameras {
        var asked: [String: Int] = [:]
    }

    private struct Stage: View {
        let roster: Roster
        let source: CallTileSource

        var body: some View {
            let status: CallSessionStatus = .connected
            let presentation = CallStagePresentation(viewMode: .speaker)
            let controls = CallControls(
                devices: .init(hasMedia: status.hasMedia),
                viewMode: presentation.viewMode, isSelfViewHidden: presentation.isSelfViewHidden,
                showsParticipants: false, canReact: status.hasMedia
            )
            CallStageView(
                title: "#design", subtitle: "2 others in call", status: status, tiles: roster.tiles,
                presentation: presentation, controls: controls, tileSource: source, announcesChanges: false,
                actions: CallStageActions(
                    toggleMicrophone: {}, toggleCamera: {}, toggleHand: {}, setViewMode: { _ in }, toggleSelfView: {},
                    hangUp: {}
                )
            )
        }
    }

    /// A picture is fetched when its camera changes, never because someone
    /// started speaking: not the speaker's own picture, and not the others'.
    func testASpeakingChangeFetchesNoPictureAgain() async throws {
        let cameras = Cameras()
        let source = CallTileSource(
            video: { tile in
                cameras.asked[tile.identity, default: 0] += 1
                return AnyView(Color.blue)
            },
            togglePin: { _ in /* drawn only */ }
        )
        let roster = Roster([Self.you, Self.tile("linus"), Self.tile("grace")])
        let window = host(Stage(roster: roster, source: source))
        defer { close(window) }
        try await settle()
        XCTAssertEqual(Set(cameras.asked.keys), Set(roster.tiles.map(\.identity)), "every tile on stage shows its picture")

        func speak(_ index: Int, _ name: String) async throws {
            roster.tiles[index] = Self.tile(name, isSpeaking: true)
            try await settle()
            roster.tiles[index] = Self.tile(name)
            try await settle()
        }
        // The stage's first update still evaluates a picture once more;
        // from then on they are compared. A call is past that at once.
        try await speak(1, "linus")
        try await speak(2, "grace")
        let drawn = cameras.asked
        for _ in 0..<3 {
            try await speak(1, "linus")
            try await speak(2, "grace")
        }
        XCTAssertEqual(cameras.asked, drawn)

        // A camera that goes off and on again is the one picture fetched again.
        roster.tiles[2] = Self.tile("grace", hasVideo: false)
        try await settle()
        roster.tiles[2] = Self.tile("grace")
        try await settle()
        var expected = drawn
        expected[roster.tiles[2].identity, default: 0] += 1
        XCTAssertEqual(cameras.asked, expected)
    }

    /// An update that leaves the choices as they were tells no view to redraw.
    func testAnUnchangedUpdateDoesNotNotify() {
        let sample = SampleSession()
        let model = sample.callStage
        let linus = Self.tile("linus")
        model.update("c1") {
            $0.speakersChanged([linus.identity])
            $0.tilesChanged([Self.you, linus], rosterIsAvailable: true)
        }

        let notified = expectation(description: "the stage is told of a change")
        notified.isInverted = true
        withObservationTracking {
            _ = model.stage(of: "c1")
        } onChange: {
            notified.fulfill()
        }
        model.update("c1") {
            // Someone else joins in, the speaker pauses, and the same people are still there.
            $0.speakersChanged([linus.identity, "grace@waddle.test/web"])
            $0.speakersChanged([])
            $0.tilesChanged([Self.you, Self.tile("linus", isSpeaking: true)], rosterIsAvailable: true)
        }
        wait(for: [notified], timeout: 0)

        let pinned = expectation(description: "a pin is a change")
        withObservationTracking {
            _ = model.stage(of: "c1")
        } onChange: {
            pinned.fulfill()
        }
        model.update("c1") { $0.togglePin(linus.id) }
        wait(for: [pinned], timeout: 0)
        XCTAssertEqual(model.stage(of: "c1").presentation.pinnedTileID, linus.id)
    }

    // MARK: - Hosting

    private func settle() async throws {
        try await Task.sleep(for: .milliseconds(300))
    }

    #if os(macOS)
    private func host(_ view: some View) -> NSWindow {
        let window = NSWindow(
            contentRect: CGRect(x: -20_000, y: -20_000, width: 400, height: 900), styleMask: [.titled],
            backing: .buffered, defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentViewController = NSHostingController(rootView: view.frame(width: 400, height: 900))
        window.orderFront(nil)
        return window
    }

    private func close(_ window: NSWindow) {
        window.close()
    }
    #else
    private func host(_ view: some View) -> UIWindow {
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        let window = scene.map(UIWindow.init(windowScene:)) ?? UIWindow()
        window.frame = CGRect(x: 0, y: 0, width: 400, height: 900)
        window.rootViewController = UIHostingController(rootView: view)
        window.isHidden = false
        window.layoutIfNeeded()
        return window
    }

    private func close(_ window: UIWindow) {
        window.isHidden = true
    }
    #endif
}
