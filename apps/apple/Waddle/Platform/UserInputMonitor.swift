import Foundation
#if os(macOS)
import AppKit
import CoreGraphics
#else
import UIKit
#endif

/// Tells the session when the user last used this device, for auto-away
/// (ADR-010), and prompts it to work out its presence again.
///
/// A Mac knows when its keyboard and pointer were last used, in any app.
/// An iPhone or iPad does not: there it is touches in this app's windows,
/// so reading without touching the screen counts as idle.
@MainActor
final class UserInputMonitor {
    /// How often presence is worked out again while nothing happens.
    private static let interval: TimeInterval = 30
    /// Input in this app prompts a refresh at once, at most this often.
    private static let promptInterval: TimeInterval = 5
    /// The timer may run this late, to share a wake-up with other timers.
    private static let tolerance: TimeInterval = 5

    /// The user was active at this instant.
    var onInput: ((Date) -> Void)?
    /// Time has passed or input was seen: presence may have changed.
    var onRefresh: (() -> Void)?

    private var timer: Timer?
    private var lastPrompt = Date.distantPast
    #if os(macOS)
    private var eventMonitor: Any?
    #else
    private var observedWindows: [ObjectIdentifier: TouchObserver] = [:]
    #endif

    func start() {
        guard timer == nil else { return }
        let timer = Timer(timeInterval: Self.interval, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        timer.tolerance = Self.tolerance
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        #if os(macOS)
        eventMonitor = NSEvent.addLocalMonitorForEvents(
            matching: [.keyDown, .leftMouseDown, .rightMouseDown, .scrollWheel]
        ) { [weak self] event in
            self?.inputSeen()
            return event
        }
        #endif
    }

    /// The app came to the front, which only the user does.
    func appBecameActive() {
        #if os(iOS)
        observeWindows()
        #endif
        lastPrompt = .distantPast
        inputSeen()
    }

    private func tick() {
        #if os(macOS)
        // Seconds since the last key, click or pointer move in any app.
        let anyInput = CGEventType(rawValue: ~0) ?? .null
        let idle = CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: anyInput)
        if idle.isFinite, idle >= 0 {
            onInput?(Date().addingTimeInterval(-idle))
        }
        #endif
        onRefresh?()
    }

    private func inputSeen() {
        let now = Date()
        onInput?(now)
        guard now.timeIntervalSince(lastPrompt) >= Self.promptInterval else { return }
        lastPrompt = now
        onRefresh?()
    }

    #if os(iOS)
    /// Windows come and go with scenes; each gets one observer.
    private func observeWindows() {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
        for window in windows where observedWindows[ObjectIdentifier(window)] == nil {
            let observer = TouchObserver { [weak self] in self?.inputSeen() }
            window.addGestureRecognizer(observer)
            observedWindows[ObjectIdentifier(window)] = observer
        }
        observedWindows = observedWindows.filter { $0.value.view != nil }
    }
    #endif
}

#if os(iOS)
/// Sees every touch in a window and takes none: it never recognizes, so
/// the window's own gestures and controls behave as before.
private final class TouchObserver: UIGestureRecognizer {
    private let onTouch: () -> Void

    init(onTouch: @escaping () -> Void) {
        self.onTouch = onTouch
        super.init(target: nil, action: nil)
        cancelsTouchesInView = false
        delaysTouchesBegan = false
        delaysTouchesEnded = false
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        onTouch()
        state = .failed
    }
}
#endif
