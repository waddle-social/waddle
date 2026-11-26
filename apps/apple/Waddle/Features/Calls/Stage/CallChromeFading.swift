import SwiftUI
import WaddleKit

extension FocusedValues {
    /// The keyboard is on one of a call's controls (web `chromeHasFocus`).
    @Entry var callChromeHasKeyboard: Bool?
}

extension View {
    /// The header or the control bar of a full-screen call, lying over the
    /// tiles: it fades and slips towards its edge while hidden (web
    /// `chromeHiddenClass`). It stays in the view tree, so the keyboard
    /// and assistive technology still reach it, and reaching it shows it.
    func callChrome(isHidden: Bool, edge: VerticalEdge) -> some View {
        modifier(CallChrome(isHidden: isHidden, edge: edge))
    }

    /// Fades a full-screen call's header and controls once the pointer has
    /// rested, and brings them back when it moves, on a click or tap and on
    /// a key (web `revealImmersiveChrome`).
    ///
    /// - Parameters:
    ///   - isOn: the call is shown full screen.
    ///   - isHeld: something open keeps them on screen, as the participants panel does.
    /// Moves with a full-screen call's header and controls as they come and go; at once with Reduce Motion.
    func callChromeMotion(value isHidden: Bool) -> some View {
        modifier(CallChromeMotion(isHidden: isHidden))
    }

    func callChromeFade(_ fader: CallChromeFader, isOn: Bool, isHeld: Bool) -> some View {
        modifier(CallChromeFading(fader: fader, isOn: isOn, isHeld: isHeld))
    }
}

private struct CallChrome: ViewModifier {
    let isHidden: Bool
    let edge: VerticalEdge
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            // Over a picture, not the stage's ground: a scrim keeps the words readable.
            .background {
                LinearGradient(
                    colors: [Theme.Colors.seaDeep.opacity(0.85), Theme.Colors.seaDeep.opacity(0)],
                    startPoint: edge == .top ? .top : .bottom, endPoint: edge == .top ? .bottom : .top
                )
                .allowsHitTesting(false)
            }
            .opacity(isHidden ? 0 : 1)
            .offset(y: isHidden && !reduceMotion ? (edge == .top ? -Metrics.slip : Metrics.slip) : 0)
            .allowsHitTesting(!isHidden)
            .callChromeMotion(value: isHidden)
    }

    /// The web's `translateY(8px)`.
    private enum Metrics {
        static let slip: CGFloat = 8
    }
}

private struct CallChromeMotion: ViewModifier {
    let isHidden: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        // The web's `180ms ease`. The ticket asks for no fade at all with Reduce Motion.
        content.animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: isHidden)
    }
}

/// Whether a full-screen call's header and controls are on screen, for
/// one window's stage. A stir only notes its time (`CallChromeFade`):
/// nothing is redrawn until they actually come or go, so a pointer
/// crossing the stage costs the video nothing (#2625).
@MainActor @Observable
final class CallChromeFader {
    private(set) var isShown = true
    @ObservationIgnored private var fade = CallChromeFade()
    @ObservationIgnored private var wait: Task<Void, Never>?
    @ObservationIgnored private var isWatching = false
    /// Something keeps them on screen now. What begins to hold them shows
    /// them, and they stay a rest longer once it has gone.
    @ObservationIgnored var isHeld = false {
        didSet { if isHeld != oldValue { stir() } }
    }
    #if os(macOS)
    @ObservationIgnored private var monitor: Any?
    @ObservationIgnored private var observers: [any NSObjectProtocol] = []
    /// A menu is open: the pointer is in it, and its moves are not seen here.
    @ObservationIgnored private var isMenuOpen = false
    #endif

    private static var now: Double { ProcessInfo.processInfo.systemUptime }

    /// The pointer moved, a key went down, or something on the stage was used.
    func stir() {
        guard isWatching else { return }
        fade.stirred(at: Self.now)
        publish()
        guard wait == nil else { return }
        wait = Task { [weak self] in
            while let delay = self?.check() {
                try? await Task.sleep(for: .seconds(delay))
                if Task.isCancelled { return }
            }
        }
    }

    /// `nil` once they are hidden: the wait is over until the next stir.
    private func check() -> Double? {
        var held = isHeld
        #if os(macOS)
        held = held || isMenuOpen
        #endif
        let delay = fade.check(at: Self.now, isHeld: held)
        publish()
        if delay == nil { wait = nil }
        return delay
    }

    /// Only a change is told to whoever draws them.
    private func publish() {
        if isShown != fade.isShown { isShown = fade.isShown }
    }

    /// The call went full screen.
    func start() {
        guard !isWatching else { return }
        isWatching = true
        #if os(macOS)
        // Any key, as on the web; the pointer is the stage's own to watch.
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            MainActor.assumeIsolated { self?.stir() }
            return event
        }
        let center = NotificationCenter.default
        observers = [
            center.addObserver(forName: NSMenu.didBeginTrackingNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.isMenuOpen = true }
            },
            center.addObserver(forName: NSMenu.didEndTrackingNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.isMenuOpen = false
                    self?.stir()
                }
            },
        ]
        #endif
        stir()
    }

    /// The call stepped down, or its stage went away.
    func stop() {
        guard isWatching else { return }
        isWatching = false
        wait?.cancel()
        wait = nil
        fade = CallChromeFade()
        publish()
        #if os(macOS)
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        isMenuOpen = false
        #endif
    }
}

private struct CallChromeFading: ViewModifier {
    let fader: CallChromeFader
    let isOn: Bool
    let isHeld: Bool
    @FocusedValue(\.callChromeHasKeyboard) private var hasKeyboard
    @Environment(\.accessibilityVoiceOverEnabled) private var voiceOver
    @Environment(\.accessibilitySwitchControlEnabled) private var switchControl

    /// Someone who cannot move a pointer over the stage must never have to
    /// find controls that are not there.
    private var holds: Bool {
        isHeld || hasKeyboard == true || voiceOver || switchControl
    }

    func body(content: Content) -> some View {
        content
            .onContinuousHover { phase in
                if case .active = phase { fader.stir() }
            }
            // A finger has no pointer to rest; a click may come with no move before it.
            .simultaneousGesture(TapGesture().onEnded { fader.stir() })
            .onChange(of: isOn, initial: true) { _, isOn in
                if isOn { fader.start() } else { fader.stop() }
            }
            .onChange(of: holds, initial: true) { _, holds in
                fader.isHeld = holds
            }
            // A stage that comes back still full screen fades again.
            .onAppear { if isOn { fader.start() } }
            .onDisappear { fader.stop() }
    }
}
