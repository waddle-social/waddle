import SwiftUI
import WaddleKit

/// How one window is laid out for calls. Each window has its own sidebar
/// and its own pane, so each decides for itself whether its dock, its
/// sidebar's card or a stage has the call; the call's own choices, which
/// are the same in every window, are in `CallStageModel`.
@MainActor @Observable
final class CallWindowLayout {
    /// The pane a stage shares with its conversation, once the shell has
    /// laid it out: the dock and the stage both place the call by it.
    var pane: CGSize?
    /// The sidebar is on screen to hold the card of a call whose conversation
    /// is not open: its column is shown and the sidebar itself says it is there.
    var isSidebarShown: Bool { sidebarColumnIsShown && sidebarIsOnScreen }
    var sidebarColumnIsShown = true
    /// Assumed until the sidebar has been laid out.
    var sidebarIsOnScreen = true
    /// Where the split shell draws calls, decided once at its root: a
    /// split view's sidebar column is compact on an iPad, and the card in
    /// it must still agree with the dock about who has the call.
    var shellPlatform = CallSurface.Platform.phone

    init() { /* A window not yet laid out. */ }
}
