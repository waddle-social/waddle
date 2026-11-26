import SwiftUI
import WaddleKit

/// A call in session as a stage (web `CallSplitContainer`): who it is with,
/// everyone's tile, and the controls. It keeps the Night ground in both
/// appearances, as a cinema does: pictures read best on dark.
///
/// Pure: everything it shows is passed in, so it renders without a session.
struct CallStageView: View {
    /// The peer, or the room.
    let title: String
    let subtitle: String
    let status: CallSessionStatus
    /// When media connected, for the header's clock; `nil` shows "In call" with no clock.
    var connectedSince: Date?
    let tiles: [CallStageTile]
    var presentation = CallStagePresentation()
    /// Who the speaker view shows large.
    var promotedSpeaker: String?
    var controls = CallControls()
    /// The participants panel's rows while it is open.
    var participants: [CallParticipantRow]?
    /// Someone's volume has been moved, whether or not they are listed now.
    var canResetVolumes = false
    /// The stage is too narrow for tiles and the panel side by side: the panel takes the tiles' place.
    var participantsCoverTiles = false
    /// What the call could not use of our own devices, each with a way to try again or put it away.
    var issues: [CallMediaIssue] = []
    /// The kinds being tried again now.
    var checkingIssues: Set<CallMediaIssue.Kind> = []
    /// How good our own connection is, once the call is live; `nil` shows no indicator.
    var connection: CallConnectionQuality?
    /// The emoji rising over the stage; `nil` draws none. Handed on whole:
    /// only the overlay reads what is in it, so only the overlay redraws.
    var reactions: CallReactions?
    /// Our nick in the room of a room's call; `nil` in a call of two.
    var ownNick: String?
    /// Our own shared screen while we share one, for the strip that shows us what everyone sees.
    var ownScreen: CallStageTile?
    /// The pointer has rested on a full-screen call: its header and controls are faded out.
    var isChromeHidden = false
    /// Where the tiles get their pictures and send a pin.
    let tileSource: CallTileSource
    /// Off in a snapshot, which has nobody to speak to.
    var announcesChanges = true
    let perform: @MainActor (CallStageAction) -> Void

    /// The letter keys that do something on this stage as it is.
    private var shortcuts: [CallShortcut] {
        CallShortcut.available(for: controls)
    }

    /// In full screen the header and controls lie over the tiles, so the
    /// picture has the whole window once they fade (web `chromeOverlayClass`).
    /// An open participants panel holds them on screen anyway, and keeps its
    /// place between them.
    private var overlaysChrome: Bool {
        controls.size == .immersive && participants == nil
    }

    private var header: some View {
        CallStageHeader(
            title: title, subtitle: subtitle, status: status, connectedSince: connectedSince, connection: connectionChip
        )
    }

    /// What the call could not use, and our own shared screen: never faded.
    @ViewBuilder
    private var notices: some View {
        if !issues.isEmpty {
            CallMediaNotice(
                issues: issues, checking: checkingIssues, perform: perform
            )
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.bottom, Theme.Spacing.s)
        }
        if let ownScreen {
            CallSelfShareNotice(screen: ownScreen, source: tileSource)
                .padding(.horizontal, Theme.Spacing.l)
                .padding(.bottom, Theme.Spacing.s)
        }
    }

    private var controlBar: some View {
        CallControlBar(controls: controls, perform: perform)
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.top, Theme.Spacing.m)
            .padding(.bottom, Theme.Spacing.l)
            .focusedValue(\.callChromeHasKeyboard, true)
    }

    var body: some View {
        // The tiles keep one place in the tree whichever way the header
        // and controls are laid out: a step into full screen must not
        // rebuild a video view.
        VStack(spacing: 0) {
            if !overlaysChrome {
                header
                notices
            }
            HStack(spacing: Theme.Spacing.m) {
                if participants == nil || !participantsCoverTiles {
                    CallTileArea(
                        tiles: tiles, presentation: presentation, promoted: promotedSpeaker, source: tileSource,
                        toggleParticipants: { perform(.toggleParticipants) }
                    )
                    .overlay {
                        if let reactions { CallReactionsOverlay(reactions: reactions, ownNick: ownNick) }
                    }
                }
                if let participants {
                    CallParticipantsPanel(
                        rows: participants, canResetVolumes: canResetVolumes, perform: perform
                    )
                    .frame(width: participantsCoverTiles ? nil : CallParticipantsPanel.width)
                    .transition(.opacity)
                }
            }
            .huddleAnimation(.settle, value: participants == nil)
            .padding(.horizontal, overlaysChrome ? Theme.Spacing.s : Theme.Spacing.l)
            .padding(.top, overlaysChrome ? Theme.Spacing.s : WaddleTokens.Spacing.s1Half)
            .padding(.bottom, overlaysChrome ? Theme.Spacing.s : Theme.Spacing.m)
            if !overlaysChrome { controlBar }
        }
        .overlay(alignment: .top) {
            if overlaysChrome {
                VStack(spacing: 0) {
                    header.callChrome(isHidden: isChromeHidden, edge: .top)
                    notices
                }
            }
        }
        .overlay(alignment: .bottom) {
            if overlaysChrome { controlBar.callChrome(isHidden: isChromeHidden, edge: .bottom) }
        }
        .background(Theme.Colors.seaDeep)
        .background(CallShortcutKeys(shortcuts: shortcuts) { perform($0.action(size: controls.size)) })
        .environment(\.huddleGround, Theme.Colors.seaDeep)
        .environment(\.colorScheme, .dark)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call with \(title)"))
        .onChange(of: announcement, initial: true) { _, text in
            if announcesChanges { AccessibilityNotification.Announcement(text).post() }
        }
        // A notice about our own devices is said as it appears (web: a polite live region).
        .onChange(of: issues) { before, now in
            guard announcesChanges else { return }
            for issue in now where !before.contains(issue) {
                AccessibilityNotification.Announcement(issue.message()).post()
            }
        }
        // Only a connection that has gone wrong is spoken, and only as it
        // does: excellent and good coming and going would be noise.
        .onChange(of: connectionChip?.label) { _, label in
            if announcesChanges, let label { AccessibilityNotification.Announcement(label).post() }
        }
    }

    /// The indicator of a live call. While it reconnects, the header's own status says so.
    private var connectionChip: CallConnectionChip? {
        guard status == .connected, let connection else { return nil }
        return CallConnectionChip(quality: connection)
    }

    /// What VoiceOver says when the status changes; never the ticking clock.
    private var announcement: String { status.announcement }
}

/// Our own connection in a call's header (web `CallConnectionIndicator`):
/// three rising bars, quiet while it is healthy, and a tinted pill with a
/// label once it is not.
struct CallConnectionIndicator: View {
    let chip: CallConnectionChip

    var body: some View {
        HStack(spacing: WaddleTokens.Spacing.s1Half) {
            HStack(alignment: .bottom, spacing: Metrics.gap) {
                ForEach(1...3, id: \.self) { bar in
                    RoundedRectangle(cornerRadius: 1)
                        .fill(bar <= chip.bars ? color : Theme.Colors.fgSoft.opacity(0.35))
                        .frame(width: Metrics.bar, height: Metrics.step * CGFloat(bar) + Metrics.base)
                }
            }
            if let label = chip.label {
                Text(label)
                    .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.xs))
                    .lineLimit(1)
            }
        }
        .foregroundStyle(color)
        .padding(.horizontal, chip.label == nil ? 0 : WaddleTokens.Spacing.s2Half)
        .frame(height: Metrics.height)
        .background { if chip.label != nil { Capsule().fill(color.opacity(0.14)) } }
        .accessibilityElement()
        .accessibilityLabel(Text(chip.status))
        .help(chip.status)
    }

    private var color: Color {
        if chip.isHealthy { return Theme.Colors.presenceAvailable }
        return switch chip.tone {
        case .neutral: Theme.Colors.fgMuted
        case .warning: Theme.Colors.warningText
        case .danger: Theme.Colors.dangerText
        }
    }

    private enum Metrics {
        static let height: CGFloat = 28
        static let bar: CGFloat = 3
        static let gap: CGFloat = 2
        static let base: CGFloat = 2
        static let step: CGFloat = 3
    }
}

/// The top of the stage (web `CallStageHeader`): a dot that turns green once
/// the call is live, the title over its subtitle, and the status or clock.
private struct CallStageHeader: View {
    let title: String
    let subtitle: String
    let status: CallSessionStatus
    /// Used only while connected: a call that is connecting or reconnecting shows no clock.
    var connectedSince: Date?
    var connection: CallConnectionChip?

    var body: some View {
        HStack(spacing: WaddleTokens.Spacing.s2Half) {
            dot
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(title)
                    .huddleText(Theme.Typography.subtitle.sized(WaddleTokens.FontSize.md))
                    .foregroundStyle(Theme.Colors.fg)
                    .accessibilityAddTraits(.isHeader)
                Text(subtitle)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .lineLimit(1)
            Spacer(minLength: Theme.Spacing.s)
            // Never squeezed: a long room name gives way, not the word that the connection is lost.
            if let connection { CallConnectionIndicator(chip: connection).fixedSize() }
            statusText
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.xs))
                .monospacedDigit()
                .lineLimit(1)
                .layoutPriority(1)
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.m)
        .frame(minHeight: Metrics.height)
        .accessibilityElement(children: .combine)
    }

    private var isLive: Bool {
        status == .connected
    }

    private var dot: some View {
        Circle()
            .fill(isLive ? Theme.Colors.presenceAvailable : Theme.Colors.fgSoft)
            .frame(width: Metrics.dot, height: Metrics.dot)
            .background {
                Circle()
                    .fill(Theme.Colors.presenceAvailable.opacity(isLive ? 0.2 : 0))
                    .padding(-Metrics.dotHalo)
            }
            .accessibilityHidden(true)
    }

    @ViewBuilder
    private var statusText: some View {
        switch (status, connectedSince) {
        case (.connecting, _):
            Text("Connecting…").foregroundStyle(Theme.Colors.fgMuted)
        case (.reconnecting, _):
            Text("Reconnecting…").foregroundStyle(Theme.Colors.warningText)
        case (.connected, nil):
            Text("In call").foregroundStyle(Theme.Colors.fgMuted)
        case (.connected, let since?):
            // Only the clock is redrawn each second, never the tiles.
            TimelineView(.periodic(from: since, by: 1)) { context in
                Text(CallClock.string(for: .seconds(context.date.timeIntervalSince(since))))
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .accessibilityLabel(Text("In call"))
                    .accessibilityAddTraits(.updatesFrequently)
            }
        }
    }

    private enum Metrics {
        /// The web header: two lines of text on its padding.
        static let height: CGFloat = 62
        static let dot: CGFloat = 10
        static let dotHalo: CGFloat = 4
    }
}
