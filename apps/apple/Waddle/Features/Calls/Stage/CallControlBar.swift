import SwiftUI
import WaddleKit

/// What the controls and tiles of a call's stage do.
struct CallStageActions {
    let toggleMicrophone: @MainActor () -> Void
    let toggleCamera: @MainActor () -> Void
    let toggleHand: @MainActor () -> Void
    let setViewMode: @MainActor (CallStagePresentation.ViewMode) -> Void
    let toggleSelfView: @MainActor () -> Void
    let hangUp: @MainActor () -> Void
}

/// The controls under a call's stage (web `CallControls` in the split):
/// round glyph buttons, the gallery and speaker switch, and the red pill
/// that ends our part of the call, "Hang up" in a room too, as on the web. When they do not fit one row the pill
/// takes a row of its own.
struct CallControlBar: View {
    var isMicrophoneOn = false
    var isCameraOn = false
    /// The devices can be switched: media has connected.
    var hasMedia = true
    /// Whether our hand is up; `nil` in a call with no hands to raise, a 1:1.
    var isHandRaised: Bool?
    var viewMode = CallStagePresentation.ViewMode.gallery
    var isSelfViewHidden = false
    let actions: CallStageActions

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: Theme.Spacing.s) {
                toggles
                endButton
            }
            VStack(spacing: Theme.Spacing.s) {
                HStack(spacing: Theme.Spacing.s) { toggles }
                endButton
            }
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call controls"))
    }

    @ViewBuilder
    private var toggles: some View {
        CallControlButton(
            title: isMicrophoneOn ? "Mute" : "Unmute", symbol: isMicrophoneOn ? "mic" : "mic.slash",
            tone: isMicrophoneOn || !hasMedia ? .plain : .off, action: actions.toggleMicrophone
        )
        .disabled(!hasMedia)
        CallControlButton(
            title: isCameraOn ? "Turn camera off" : "Turn camera on", symbol: isCameraOn ? "video" : "video.slash",
            tone: isCameraOn || !hasMedia ? .plain : .chosen, action: actions.toggleCamera
        )
        .disabled(!hasMedia)
        viewSwitch
        if let isHandRaised {
            CallControlButton(
                title: isHandRaised ? "Lower hand" : "Raise hand", symbol: "hand.raised",
                tone: isHandRaised ? .chosen : .plain, action: actions.toggleHand
            )
            .accessibilityAddTraits(isHandRaised ? .isSelected : [])
            .disabled(!hasMedia)
        }
        more
    }

    // MARK: - Gallery or speaker

    private var viewSwitch: some View {
        HStack(spacing: 0) {
            viewChoice(.gallery, title: "Gallery view", symbol: "square.grid.2x2")
            viewChoice(.speaker, title: "Speaker view", symbol: "person.crop.square")
        }
        .padding(Metrics.switchInset)
        .background(Capsule().fill(Theme.Colors.surface2))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Stage view"))
    }

    private func viewChoice(_ mode: CallStagePresentation.ViewMode, title: String, symbol: String) -> some View {
        let isChosen = viewMode == mode
        let side = CallControlButton.side - 2 * Metrics.switchInset
        return Button { actions.setViewMode(mode) } label: {
            Image(systemName: symbol)
                .font(.system(size: Metrics.switchGlyph, weight: .medium))
                .foregroundStyle(isChosen ? Theme.Colors.fg : Theme.Colors.fgMuted)
                .frame(width: side, height: side)
                .background {
                    if isChosen {
                        Circle().fill(Theme.Colors.surface)
                            .overlay(Circle().strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair))
                    }
                }
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(title))
        .accessibilityAddTraits(isChosen ? .isSelected : [])
        .help(title)
    }

    // MARK: - More

    private var more: some View {
        Menu {
            Button(isSelfViewHidden ? "Show self view" : "Hide self view", action: actions.toggleSelfView)
        } label: {
            CallControlButton.face(symbol: "ellipsis", tone: .plain)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(Text("More options"))
        .help("More options")
    }

    // MARK: - End

    private var endButton: some View {
        Button(action: actions.hangUp) {
            Label("Hang up", systemImage: "phone.down")
                .huddleText(Theme.Typography.button.weight(700))
                .foregroundStyle(Theme.Colors.dangerFg)
                .padding(.horizontal, Theme.Spacing.l)
                .frame(height: CallControlButton.side)
                .background(Capsule().fill(Theme.Colors.danger))
                .contentShape(Capsule())
        }
        .buttonStyle(CallControlPressStyle())
        .help("Hang up")
    }

    private enum Metrics {
        static let switchInset: CGFloat = 2
        static let switchGlyph: CGFloat = 16
    }
}

/// A round, glyph-only call control (web `huddleCtl`).
struct CallControlButton: View {
    enum Tone {
        case plain
        /// Something of ours is off and others notice: the microphone.
        case off
        /// A choice that is in effect: the camera off, a hand up.
        case chosen
    }

    let title: String
    let symbol: String
    var tone = Tone.plain
    let action: @MainActor () -> Void

    static let side = WaddleTokens.ControlHeight.touch

    var body: some View {
        Button(action: action) { Self.face(symbol: symbol, tone: tone) }
            .buttonStyle(CallControlPressStyle())
            .accessibilityLabel(Text(title))
            .help(title)
    }

    static func face(symbol: String, tone: Tone) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 17, weight: .medium))
            .foregroundStyle(foreground(tone))
            .frame(width: side, height: side)
            .background(Circle().fill(background(tone)))
            .contentShape(Circle())
    }

    private static func foreground(_ tone: Tone) -> Color {
        switch tone {
        case .plain: Theme.Colors.fg
        case .off: Theme.Colors.dangerText
        case .chosen: Theme.Colors.actionText
        }
    }

    private static func background(_ tone: Tone) -> Color {
        switch tone {
        case .plain: Theme.Colors.surface2
        case .off: Theme.Colors.danger.opacity(0.18)
        case .chosen: Theme.Colors.actionSubtle
        }
    }
}

/// The press of a call control: the web's hover layer and press scale.
private struct CallControlPressStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .overlay(Capsule().fill(configuration.isPressed ? Theme.State.press : .clear))
            .scaleEffect(configuration.isPressed && !reduceMotion ? HuddleMotion.pressScale : 1)
            .opacity(isEnabled ? 1 : 0.5)
            .huddleAnimation(.press, value: configuration.isPressed)
    }
}
