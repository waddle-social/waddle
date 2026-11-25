import SwiftUI
import WaddleKit

/// The controls under a call's stage (web `CallControls` in the split):
/// round glyph buttons, the gallery and speaker switch, and the red pill
/// that ends our part of the call, "Hang up" in a room too, as on the web. When they do not fit one row the pill
/// takes a row of its own.
struct CallControlBar: View {
    var controls = CallControls()
    let perform: @MainActor (CallStageAction) -> Void

    private var isMicrophoneOn: Bool { controls.devices.isMicrophoneOn }
    private var isCameraOn: Bool { controls.devices.isCameraOn }
    private var hasMedia: Bool { controls.devices.hasMedia }
    private var isHandRaised: Bool? { controls.isHandRaised }
    private var viewMode: CallStagePresentation.ViewMode { controls.viewMode }
    private var isSelfViewHidden: Bool { controls.isSelfViewHidden }
    private var size: CallSurface.Size { controls.size }
    private var escapeStepsDown: Bool { controls.escapeStepsDown }
    private var showsParticipants: Bool { controls.showsParticipants }
    private var isSharingScreen: Bool? { controls.devices.isSharingScreen }
    private var chatUnread: Int? { controls.chatUnread }
    private var showsChat: Bool { controls.showsChat }
    private var canReact: Bool { controls.canReact }

    var body: some View {
        // Widest first; the bar takes the first that fits (`CallControlBarLayout`).
        ViewThatFits(in: .horizontal) {
            row(.oneRow)
            row(.twoRows)
            row(.viewChoiceInMenu)
            row(.reactionsInMenu)
            row(.extrasInMenu)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call controls"))
    }

    @ViewBuilder
    private func row(_ layout: CallControlBarLayout) -> some View {
        if layout.stacksEndButton {
            VStack(spacing: Theme.Spacing.s) {
                HStack(spacing: Theme.Spacing.s) { toggles(layout) }
                endButton
            }
        } else {
            HStack(spacing: Theme.Spacing.s) {
                toggles(layout)
                endButton
            }
        }
    }

    private func isInBar(_ control: CallControlBarLayout.Control, _ layout: CallControlBarLayout) -> Bool {
        layout.place(of: control, for: controls) == .bar
    }

    private func isInMenu(_ control: CallControlBarLayout.Control, _ layout: CallControlBarLayout) -> Bool {
        layout.place(of: control, for: controls) == .menu
    }

    @ViewBuilder
    private func toggles(_ layout: CallControlBarLayout) -> some View {
        CallControlButton(
            title: isMicrophoneOn ? "Mute" : "Unmute", symbol: isMicrophoneOn ? "mic" : "mic.slash",
            tone: isMicrophoneOn || !hasMedia ? .plain : .off, action: { perform(.toggleMicrophone) }
        )
        .disabled(!hasMedia)
        CallControlButton(
            title: isCameraOn ? "Turn camera off" : "Turn camera on", symbol: isCameraOn ? "video" : "video.slash",
            tone: isCameraOn || !hasMedia ? .plain : .chosen, action: { perform(.toggleCamera) }
        )
        .disabled(!hasMedia)
        if isInBar(.viewChoice, layout) { viewSwitch }
        if let isHandRaised, isInBar(.hand, layout) {
            CallControlButton(
                title: isHandRaised ? "Lower hand" : "Raise hand", symbol: "hand.raised",
                tone: isHandRaised ? .chosen : .plain, action: { perform(.toggleHand) }
            )
            .accessibilityAddTraits(isHandRaised ? .isSelected : [])
            .disabled(!hasMedia)
        }
        if let isSharingScreen, isInBar(.screenShare, layout) {
            // A share in progress keeps its button whatever the width: stopping it must be one click away.
            CallControlButton(
                title: isSharingScreen ? "Stop sharing" : "Share screen", symbol: "rectangle.inset.filled.and.person.filled",
                tone: isSharingScreen ? .chosen : .plain, action: { perform(.toggleScreenShare) }
            )
            .accessibilityAddTraits(isSharingScreen ? .isSelected : [])
            .disabled(!hasMedia)
        }
        // Unread messages, like a share in progress, keep their button whatever the width:
        // a count nobody can see is no count, and an open chat must be one click from closed.
        if let chatUnread, isInBar(.chat, layout) {
            CallControlButton(
                title: CallChat.buttonTitle(unread: chatUnread, isOpen: showsChat), symbol: "bubble.left",
                tone: showsChat ? .chosen : .plain, action: { perform(.toggleChat) }
            )
            .overlay(alignment: .topTrailing) {
                if !showsChat, let badge = CallChat.badge(chatUnread) { CallControlBadge(text: badge) }
            }
            .accessibilityAddTraits(showsChat ? .isSelected : [])
        }
        if isInBar(.react, layout) { reactMenu }
        if isInBar(.participants, layout) {
            CallControlButton(
                title: showsParticipants ? "Hide participants" : "Participants", symbol: "person.2",
                tone: showsParticipants ? .chosen : .plain, action: { perform(.toggleParticipants) }
            )
            .accessibilityAddTraits(showsParticipants ? .isSelected : [])
        }
        sizeSteps
        more(layout)
    }

    // MARK: - Split, expanded, full screen

    /// One step at a time, as on the web: split and expanded swap, and full
    /// screen is offered from expanded and offers only its way back. Escape
    /// takes the step down unless something else on screen wants it.
    @ViewBuilder
    private var sizeSteps: some View {
        if size != .immersive {
            let isExpanded = size == .expanded
            CallControlButton(
                title: isExpanded ? "Collapse call" : "Expand call",
                symbol: isExpanded ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right"
            ) { perform(.setSize(isExpanded ? .split : .expanded)) }
            .keyboardShortcut(isExpanded && escapeStepsDown ? .cancelAction : nil)
        }
        if size != .split {
            let isFullScreen = size == .immersive
            CallControlButton(
                title: isFullScreen ? "Exit full screen" : "Full screen",
                symbol: isFullScreen ? "arrow.down.right.and.arrow.up.left.rectangle" : "arrow.up.left.and.arrow.down.right.rectangle"
            ) { perform(.setSize(isFullScreen ? .expanded : .immersive)) }
            .keyboardShortcut(isFullScreen && escapeStepsDown ? .cancelAction : nil)
        }
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
        return Button { perform(.setViewMode(mode)) } label: {
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

    // MARK: - Reactions

    /// The six emoji the web's picker offers, one click each.
    private var reactMenu: some View {
        Menu {
            reactionChoices
        } label: {
            CallControlButton.face(symbol: "face.smiling", tone: .plain)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(Text("React"))
        .help("React")
    }

    private var reactionChoices: some View {
        ForEach(CallReactionLedger.offered, id: \.self) { emoji in
            Button(emoji) { perform(.react(emoji)) }
        }
    }

    private func more(_ layout: CallControlBarLayout) -> some View {
        Menu {
            if isInMenu(.screenShare, layout) {
                Button("Share screen", action: { perform(.toggleScreenShare) }).disabled(!hasMedia)
            }
            if isInMenu(.participants, layout) {
                Button(showsParticipants ? "Hide participants" : "Participants", action: { perform(.toggleParticipants) })
            }
            if isInMenu(.chat, layout) {
                Button(CallChat.buttonTitle(unread: 0, isOpen: false), action: { perform(.toggleChat) })
            }
            if isInMenu(.react, layout) {
                Menu("React") { reactionChoices }
            }
            if isInMenu(.viewChoice, layout) {
                Picker("Stage view", selection: Binding(get: { viewMode }, set: { perform(.setViewMode($0)) })) {
                    Text("Gallery view").tag(CallStagePresentation.ViewMode.gallery)
                    Text("Speaker view").tag(CallStagePresentation.ViewMode.speaker)
                }
                .pickerStyle(.inline)
            }
            Button(isSelfViewHidden ? "Show self view" : "Hide self view", action: { perform(.toggleSelfView) })
            Divider()
            Button("Call settings…", action: { perform(.openSettings) })
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
        Button(action: { perform(.hangUp) }) {
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

/// A count on the corner of a round control: messages not yet seen.
struct CallControlBadge: View {
    let text: String

    var body: some View {
        Text(text)
            .huddleText(Theme.Typography.caption.weight(700).sized(WaddleTokens.FontSize.xs))
            .monospacedDigit()
            .foregroundStyle(Theme.Colors.actionFg)
            .padding(.horizontal, WaddleTokens.Spacing.s1)
            .frame(minWidth: Metrics.side, minHeight: Metrics.side)
            .background(Capsule().fill(Theme.Colors.action))
            .offset(x: Metrics.overhang, y: -Metrics.overhang)
            // The button's own title says the count.
            .accessibilityHidden(true)
            .allowsHitTesting(false)
    }

    private enum Metrics {
        static let side: CGFloat = 18
        static let overhang: CGFloat = 4
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
