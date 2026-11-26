import SwiftUI
import WaddleKit

/// What a stage shows in its tiles' place while the call floats (web
/// `CallExpandedSurface`, `pipClass`): where the call went, and the way back.
struct CallFloatingNotice: View {
    /// Escape brings the call back, unless something else on screen wants the key.
    var returnsOnEscape = true
    let returnToCall: @MainActor () -> Void

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            Image(systemName: "pip")
                .font(.system(size: Metrics.glyph, weight: .medium))
                .foregroundStyle(Theme.Colors.fgMuted)
                .accessibilityHidden(true)
            Text("The call is in Picture-in-Picture")
                .huddleText(Theme.Typography.label)
                .foregroundStyle(Theme.Colors.fg)
            Text("Keep chatting while it floats above your other windows.")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .multilineTextAlignment(.center)
            Button("Return to call", action: returnToCall)
                .buttonStyle(.huddle(size: .small))
                .keyboardShortcut(returnsOnEscape ? .cancelAction : nil)
                .padding(.top, Theme.Spacing.xs)
        }
        .padding(Theme.Spacing.l)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(CallTileView.Metrics.shape.fill(Theme.Colors.surface))
        .overlay(CallTileView.Metrics.shape.strokeBorder(Theme.Colors.borderSoft, lineWidth: WaddleTokens.BorderWidth.hair))
        .accessibilityElement(children: .contain)
    }

    private enum Metrics {
        static let glyph: CGFloat = 28
    }
}

/// A call in the window it floats in (web `CallPictureInPicturePanel`):
/// the dark stage in miniature, one tile over a row of round controls.
///
/// Pure: everything it shows is passed in, so it renders without a session.
struct CallFloatingView: View {
    /// Whose picture floats; `nil` once nobody has one.
    let tile: CallStageTile?
    var devices = CallControls.Devices()
    let tileSource: CallTileSource
    let perform: @MainActor (CallStageAction) -> Void

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            Group {
                if let tile {
                    CallTileView(tile: tile, pins: false, source: tileSource)
                        .equatable()
                        // A change of who floats is a new picture, not the old one renamed.
                        .id(tile.id)
                        .padding(CallTileView.Metrics.overdraw)
                } else {
                    Text("No active video")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(
                            CallTileView.Metrics.shape
                                .strokeBorder(Theme.Colors.border, style: StrokeStyle(lineWidth: 1, dash: [4, 4]))
                        )
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            HStack(spacing: Theme.Spacing.s) {
                CallControlButton(
                    title: devices.isMicrophoneOn ? "Mute" : "Unmute", symbol: devices.isMicrophoneOn ? "mic" : "mic.slash",
                    tone: devices.isMicrophoneOn || !devices.hasMedia ? .plain : .off, action: { perform(.toggleMicrophone) }
                )
                .disabled(!devices.hasMedia)
                CallControlButton(
                    title: devices.isCameraOn ? "Turn camera off" : "Turn camera on",
                    symbol: devices.isCameraOn ? "video" : "video.slash",
                    tone: devices.isCameraOn || !devices.hasMedia ? .plain : .chosen, action: { perform(.toggleCamera) }
                )
                .disabled(!devices.hasMedia)
                CallControlButton(title: "Return to call", symbol: "pip.exit") { perform(.togglePictureInPicture) }
                CallControlButton(title: "Hang up", symbol: "phone.down", tone: .end) { perform(.hangUp) }
            }
        }
        .padding(Theme.Spacing.s)
        .background(Theme.Colors.seaDeep.ignoresSafeArea())
        .environment(\.huddleGround, Theme.Colors.seaDeep)
        .environment(\.colorScheme, .dark)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call Picture-in-Picture"))
    }
}
