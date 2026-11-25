import SwiftUI
import WaddleKit

extension View {
    /// Holding Space on a call's stage opens the microphone until the key
    /// comes up (web: push-to-talk), and a click on the stage takes the
    /// keyboard out of the message being written beside it, so that the
    /// call's keys are the call's. A Mac only: an iPad's keyboard does not
    /// say when a key comes up to anything but a text field.
    ///
    /// - Parameters:
    ///   - hasMedia: Media has connected: there is a microphone to open.
    ///   - coversConversation: The stage is drawn over its conversation, whose composer nobody can see.
    func callPushToTalk(
        hasMedia: Bool, isMicrophoneOn: Bool, coversConversation: Bool,
        setMicrophone: @escaping @MainActor (Bool) async -> Void
    ) -> some View {
        #if os(macOS)
        background(
            CallKeyboardReader(
                hasMedia: hasMedia, isMicrophoneOn: isMicrophoneOn, coversConversation: coversConversation,
                setMicrophone: setMicrophone
            )
        )
        #else
        self
        #endif
    }
}

#if os(macOS)
/// Watches the keys and clicks of the window a stage is in. It is the
/// stage's background, so its view's frame is the stage's.
private struct CallKeyboardReader: NSViewRepresentable {
    let hasMedia: Bool
    let isMicrophoneOn: Bool
    let coversConversation: Bool
    let setMicrophone: @MainActor (Bool) async -> Void

    func makeCoordinator() -> CallStageKeyboard { CallStageKeyboard() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        context.coordinator.start(watching: view)
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        let keyboard = context.coordinator
        keyboard.hasMedia = hasMedia
        keyboard.isMicrophoneOn = isMicrophoneOn
        keyboard.setMicrophone = setMicrophone
        keyboard.coversConversation = coversConversation
    }

    static func dismantleNSView(_ view: NSView, coordinator: CallStageKeyboard) {
        coordinator.stop()
    }
}

/// The keyboard and pointer of a stage's window, as far as a call cares:
/// Space held to talk (`CallPushToTalk`), and text focus given up when the
/// stage is clicked or grows over the conversation.
@MainActor
final class CallStageKeyboard {
    private static let spaceKeyCode: UInt16 = 49

    var hasMedia = false
    var isMicrophoneOn = false
    var setMicrophone: @MainActor (Bool) async -> Void = { _ in /* no call yet */ }
    var coversConversation = false {
        didSet { if coversConversation, !oldValue { leaveTextBehind(onlyOutsideStage: false) } }
    }

    private var talk = CallPushToTalk()
    private weak var view: NSView?
    private var monitor: Any?
    private var resignObserver: (any NSObjectProtocol)?
    /// Opening and closing in the order they were asked for: a quick tap
    /// of Space must not close the microphone before it has opened.
    private var work: Task<Void, Never>?

    func start(watching view: NSView) {
        self.view = view
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .leftMouseDown]) { [weak self] event in
            MainActor.assumeIsolated { self?.handle(event) ?? event }
        }
        resignObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didResignKeyNotification, object: nil, queue: .main
        ) { [weak self] note in
            MainActor.assumeIsolated {
                guard let self, let window = self.view?.window, note.object as? NSWindow === window else { return }
                self.letGo()
            }
        }
    }

    func stop() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        if let resignObserver { NotificationCenter.default.removeObserver(resignObserver) }
        resignObserver = nil
        // The stage went away with the key down.
        letGo()
    }

    /// What becomes of an event in this app: `nil` when it was ours alone.
    func handle(_ event: NSEvent) -> NSEvent? {
        guard let window = view?.window else { return event }
        switch event.type {
        case .leftMouseDown:
            if event.window === window, isOnStage(event.locationInWindow) {
                // After the click has been delivered: it may itself be on a field.
                DispatchQueue.main.async { [weak self] in self?.leaveTextBehind(onlyOutsideStage: true) }
            }
            return event
        case .keyDown where event.keyCode == Self.spaceKeyCode:
            // A key held while the keyboard moved to another window still repeats there.
            guard event.window === window || talk.isHolding else { return event }
            let chord: NSEvent.ModifierFlags = [.command, .control, .option]
            let outcome = talk.spaceWentDown(
                CallPushToTalk.Press(
                    isRepeat: event.isARepeat, hasModifiers: !event.modifierFlags.isDisjoint(with: chord),
                    keyboardIsElsewhere: window.firstResponder !== window, hasMedia: hasMedia,
                    isMicrophoneOn: isMicrophoneOn
                )
            )
            return act(on: outcome, event)
        case .keyUp where event.keyCode == Self.spaceKeyCode:
            return act(on: talk.spaceCameUp(), event)
        default:
            return event
        }
    }

    private func act(on outcome: CallPushToTalk.Outcome, _ event: NSEvent) -> NSEvent? {
        switch outcome {
        case .passOn: return event
        case .swallow: return nil
        case .openMicrophone:
            set(true)
            return nil
        case .closeMicrophone:
            set(false)
            return nil
        }
    }

    private func letGo() {
        if talk.release() { set(false) }
    }

    private func set(_ on: Bool) {
        let setMicrophone = setMicrophone
        work = Task { [work] in
            await work?.value
            await setMicrophone(on)
        }
    }

    private func isOnStage(_ pointInWindow: NSPoint) -> Bool {
        guard let view else { return false }
        return view.bounds.contains(view.convert(pointInWindow, from: nil))
    }

    /// Takes the keyboard out of a text field, so that the stage's keys are
    /// not typed into a message. A field on the stage itself keeps it.
    private func leaveTextBehind(onlyOutsideStage: Bool) {
        guard let view, let window = view.window, let text = window.firstResponder as? NSText else { return }
        if onlyOutsideStage {
            let frame = view.convert(text.convert(text.bounds, to: nil), from: nil)
            if view.bounds.intersects(frame) { return }
        }
        window.makeFirstResponder(nil)
    }
}
#endif
