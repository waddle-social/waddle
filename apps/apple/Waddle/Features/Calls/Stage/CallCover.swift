import SwiftUI
import WaddleKit

extension View {
    /// On a phone, a call opened from the dock has its stage over the whole
    /// screen (`CallSurface.cover`), in portrait and landscape, until it is
    /// minimised or ends. Nothing where a call has a pane to share.
    func callCover(
        session: SessionCoordinator, stage: CallStageModel, navigation: NavigationModel, media: (any CallMediaControls)?
    ) -> some View {
        #if os(iOS)
        modifier(CallCover(session: session, stage: stage, navigation: navigation, media: media))
        #else
        self
        #endif
    }
}

#if os(iOS)
private struct CallCover: ViewModifier {
    let session: SessionCoordinator
    let stage: CallStageModel
    let navigation: NavigationModel
    let media: (any CallMediaControls)?
    @CallPlatform private var platform

    func body(content: Content) -> some View {
        let state = session.calls.state
        let covers = stage.surface(of: state, open: navigation.selection, platform: platform, in: nil) == .cover
        content.fullScreenCover(isPresented: Binding(
            get: { covers },
            set: { shows in if !shows, let sid = state.sid { stage.update(sid) { $0.minimise() } } }
        )) {
            CallStageHost(coversScreen: true)
                // The stage's ground reaches under the status bar and the home indicator; its contents do not.
                .background(Theme.Colors.seaDeep.ignoresSafeArea())
                .preferredColorScheme(.dark)
                .environment(session)
                .environment(stage)
                .environment(navigation)
                .environment(\.callMediaControls, media)
        }
    }
}
#endif
