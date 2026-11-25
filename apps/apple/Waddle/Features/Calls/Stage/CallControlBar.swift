import SwiftUI
import WaddleKit

/// What the controls and tiles of a call's stage do.
struct CallStageActions {
    let toggleMicrophone: @MainActor () -> Void
    let toggleCamera: @MainActor () -> Void
    let toggleHand: @MainActor () -> Void
    let setViewMode: @MainActor (CallStagePresentation.ViewMode) -> Void
    let toggleSelfView: @MainActor () -> Void
    var setSize: @MainActor (CallSurface.Size) -> Void = { _ in /* a stage of one size */ }
    var toggleParticipants: @MainActor () -> Void = { /* a stage with no panel */ }
    var setVolume: @MainActor (_ identity: String, _ gain: Double) -> Void = { _, _ in /* nobody to turn down */ }
    var resetVolumes: @MainActor () -> Void = { /* nothing to reset */ }
    var openSettings: @MainActor () -> Void = { /* a stage with no settings */ }
    /// Sends an emoji for everyone in the call to see for a moment.
    var sendReaction: @MainActor (String) -> Void = { _ in /* a call with nobody to show it to */ }
    /// Opens the call's chat beside the conversation, or puts it away.
    var toggleChat: @MainActor () -> Void = { /* a call with no chat */ }
    /// Tries a device the call could not use again, or puts its notice away for this call.
    var retryMediaIssue: @MainActor (CallMediaIssue.Kind) -> Void = { _ in /* nothing to retry */ }
    var dismissMediaIssue: @MainActor (CallMediaIssue.Kind) -> Void = { _ in /* nothing to dismiss */ }
    /// Opens the system's settings where a device is allowed.
    var openSystemSettings: @MainActor (CallMediaIssue.Kind) -> Void = { _ in /* nowhere to open */ }
    /// Starts sharing our screen, by way of the picker, or stops it.
    var toggleScreenShare: @MainActor () -> Void = { /* a device that cannot share */ }
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
    var size = CallSurface.Size.split
    /// Off while something else on screen has a use for Escape.
    var escapeStepsDown = true
    var showsParticipants = false
    /// Whether we are sharing our screen; `nil` where this device cannot, an iPhone or iPad.
    var isSharingScreen: Bool?
    /// The messages of the call's chat not yet seen; `nil` for a call with no chat to offer.
    var chatUnread: Int?
    var showsChat = false
    /// Media has connected: there is a call to react in.
    var canReact = false
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
            // The 400 pt column of a room's call: the view choice moves into More.
            VStack(spacing: Theme.Spacing.s) {
                HStack(spacing: Theme.Spacing.s) { toggles(viewChoiceInMenu: true) }
                endButton
            }
            // One button too many for that column: reacting goes into More before sharing and the list of people do.
            VStack(spacing: Theme.Spacing.s) {
                HStack(spacing: Theme.Spacing.s) { toggles(viewChoiceInMenu: true, reactInMenu: true) }
                endButton
            }
            // Narrower still, or with every button there is: sharing and the list of people go there too.
            VStack(spacing: Theme.Spacing.s) {
                HStack(spacing: Theme.Spacing.s) { toggles(viewChoiceInMenu: true, extrasInMenu: true) }
                endButton
            }
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call controls"))
    }

    private var toggles: some View { toggles(viewChoiceInMenu: false) }

    @ViewBuilder
    private func toggles(viewChoiceInMenu: Bool, reactInMenu: Bool = false, extrasInMenu: Bool = false) -> some View {
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
        if !viewChoiceInMenu { viewSwitch }
        if let isHandRaised {
            CallControlButton(
                title: isHandRaised ? "Lower hand" : "Raise hand", symbol: "hand.raised",
                tone: isHandRaised ? .chosen : .plain, action: actions.toggleHand
            )
            .accessibilityAddTraits(isHandRaised ? .isSelected : [])
            .disabled(!hasMedia)
        }
        if let isSharingScreen, !extrasInMenu || isSharingScreen {
            // A share in progress keeps its button whatever the width: stopping it must be one click away.
            CallControlButton(
                title: isSharingScreen ? "Stop sharing" : "Share screen", symbol: "rectangle.inset.filled.and.person.filled",
                tone: isSharingScreen ? .chosen : .plain, action: actions.toggleScreenShare
            )
            .accessibilityAddTraits(isSharingScreen ? .isSelected : [])
            .disabled(!hasMedia)
        }
        // Unread messages, like a share in progress, keep their button whatever the width:
        // a count nobody can see is no count, and an open chat must be one click from closed.
        if let chatUnread, !extrasInMenu || chatUnread > 0 || showsChat {
            CallControlButton(
                title: CallChat.buttonTitle(unread: chatUnread, isOpen: showsChat), symbol: "bubble.left",
                tone: showsChat ? .chosen : .plain, action: actions.toggleChat
            )
            .overlay(alignment: .topTrailing) {
                if !showsChat, let badge = CallChat.badge(chatUnread) { CallControlBadge(text: badge) }
            }
            .accessibilityAddTraits(showsChat ? .isSelected : [])
        }
        if canReact, !extrasInMenu, !reactInMenu { reactMenu }
        if !extrasInMenu {
            CallControlButton(
                title: showsParticipants ? "Hide participants" : "Participants", symbol: "person.2",
                tone: showsParticipants ? .chosen : .plain, action: actions.toggleParticipants
            )
            .accessibilityAddTraits(showsParticipants ? .isSelected : [])
        }
        sizeSteps
        more(withViewChoice: viewChoiceInMenu, withExtras: extrasInMenu, withReact: reactInMenu || extrasInMenu)
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
            ) { actions.setSize(isExpanded ? .split : .expanded) }
            .keyboardShortcut(isExpanded && escapeStepsDown ? .cancelAction : nil)
        }
        if size != .split {
            let isFullScreen = size == .immersive
            CallControlButton(
                title: isFullScreen ? "Exit full screen" : "Full screen",
                symbol: isFullScreen ? "arrow.down.right.and.arrow.up.left.rectangle" : "arrow.up.left.and.arrow.down.right.rectangle"
            ) { actions.setSize(isFullScreen ? .expanded : .immersive) }
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
            Button(emoji) { actions.sendReaction(emoji) }
        }
    }

    private func more(withViewChoice: Bool, withExtras: Bool = false, withReact: Bool = false) -> some View {
        Menu {
            if withExtras {
                if isSharingScreen == false {
                    Button("Share screen", action: actions.toggleScreenShare).disabled(!hasMedia)
                }
                Button(showsParticipants ? "Hide participants" : "Participants", action: actions.toggleParticipants)
                if let chatUnread, chatUnread == 0, !showsChat {
                    Button(CallChat.buttonTitle(unread: 0, isOpen: false), action: actions.toggleChat)
                }
            }
            if withReact, canReact {
                Menu("React") { reactionChoices }
            }
            if withViewChoice {
                Picker("Stage view", selection: Binding(get: { viewMode }, set: { actions.setViewMode($0) })) {
                    Text("Gallery view").tag(CallStagePresentation.ViewMode.gallery)
                    Text("Speaker view").tag(CallStagePresentation.ViewMode.speaker)
                }
                .pickerStyle(.inline)
            }
            Button(isSelfViewHidden ? "Show self view" : "Hide self view", action: actions.toggleSelfView)
            Divider()
            Button("Call settings…", action: actions.openSettings)
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
