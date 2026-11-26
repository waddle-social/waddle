#if os(macOS)
import AppKit
import SwiftUI
import WaddleKit

/// The window a session's call floats in while it is in Picture-in-Picture
/// (web: a Document Picture-in-Picture window). It follows the call's own
/// choice (`CallStageMemory.isFloating`), not a view: the call floats on
/// whatever the main window shows, and with the main window closed.
@MainActor
final class CallFloatingWindow: NSObject, NSWindowDelegate {
    private let session: SessionCoordinator
    private let stage: CallStageModel
    private let media: any CallMediaControls
    private var panel: NSPanel?
    private var isClosed = false

    init(session: SessionCoordinator, stage: CallStageModel, media: any CallMediaControls) {
        self.session = session
        self.stage = stage
        self.media = media
        super.init()
        follow()
    }

    /// The call in session that floats now.
    private var floatingCall: String? {
        let state = session.calls.state
        guard state.isEstablished, let sid = state.sid, stage.stage(of: sid).isFloating else { return nil }
        return sid
    }

    private func follow() {
        guard !isClosed else { return }
        let floats = withObservationTracking { floatingCall != nil } onChange: { [weak self] in
            Task { @MainActor in self?.follow() }
        }
        if floats { show() } else { hide() }
    }

    private func show() {
        guard panel == nil else { return }
        let panel = NSPanel(
            contentRect: NSRect(origin: .zero, size: Metrics.size),
            styleMask: [.titled, .closable, .resizable, .fullSizeContentView, .nonactivatingPanel],
            backing: .buffered, defer: true
        )
        panel.title = "Call"
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.standardWindowButton(.miniaturizeButton)?.isHidden = true
        panel.standardWindowButton(.zoomButton)?.isHidden = true
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.hidesOnDeactivate = false
        // Over another application's full-screen window too, and on every Space.
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isMovableByWindowBackground = true
        panel.isReleasedWhenClosed = false
        panel.minSize = Metrics.minSize
        panel.appearance = NSAppearance(named: .darkAqua)
        panel.contentView = NSHostingView(
            rootView: CallFloatingHost()
                .environment(session)
                .environment(stage)
                .environment(\.callMediaControls, media)
        )
        // Where the person left it last; the first time, the top trailing corner of the screen.
        if !panel.setFrameUsingName(Metrics.frameName), let screen = NSScreen.main?.visibleFrame {
            panel.setFrameTopLeftPoint(NSPoint(
                x: screen.maxX - Metrics.size.width - Metrics.margin, y: screen.maxY - Metrics.margin
            ))
        }
        panel.setFrameAutosaveName(Metrics.frameName)
        panel.delegate = self
        panel.orderFrontRegardless()
        self.panel = panel
    }

    private func hide() {
        guard let panel else { return }
        self.panel = nil
        panel.delegate = nil
        panel.close()
    }

    /// The session is over.
    func close() {
        isClosed = true
        hide()
    }

    /// Its close button: the call goes back to its stage.
    func windowWillClose(_ notification: Notification) {
        panel?.delegate = nil
        panel = nil
        guard let sid = session.calls.state.sid else { return }
        stage.update(sid) { if $0.isFloating { $0.toggleFloating() } }
    }

    private enum Metrics {
        /// The web asks for 360 × 240; the title bar's strip is on top of that.
        static let size = CGSize(width: 360, height: 268)
        static let minSize = CGSize(width: 240, height: 200)
        static let margin: CGFloat = 24
        static let frameName = "CallPictureInPicture"
    }
}

/// The floating window's contents: reads the session's call into `CallFloatingView`.
private struct CallFloatingHost: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(CallStageModel.self) private var model
    @Environment(\.callMediaControls) private var media
    /// The window's own: its tile pins nothing.
    @State private var source: CallTileSource?

    var body: some View {
        let call = CallOnStage(state: session.calls.state, phase: media?.phase ?? .idle, session: session)
        let commands = CallCommands(calls: session.calls, media: media)
        CallFloatingView(
            tile: call.flatMap(tile),
            devices: .init(of: media, status: call?.status ?? .connecting),
            tileSource: source ?? model.tileSource
        ) { action in
            switch action {
            case .toggleMicrophone: commands.toggleMicrophone()
            case .toggleCamera: commands.toggleCamera()
            case .hangUp: commands.hangUp()
            case .togglePictureInPicture:
                if let sid = call?.sid { model.update(sid) { $0.toggleFloating() } }
            default: break
            }
        }
        .onAppear {
            source = CallTileSource(video: model.tileSource.video, togglePin: { _ in /* nothing to pin here */ })
        }
    }

    private func tile(of call: CallOnStage) -> CallStageTile? {
        // Nobody's speaking ring is held here: who floats follows who speaks.
        let cast = call.cast(media: media, session: session, speaking: [])
        return CallPictureInPicture.tile(
            in: cast.tiles, presentation: model.stage(of: call.sid).presentation,
            activeSpeakers: media?.activeSpeakersLoudestFirst ?? []
        )
    }
}
#endif
