import SwiftUI
import WaddleKit

/// How a call in session is doing, for the stage's header.
enum CallStageStatus: Equatable {
    case connecting
    /// Media is up, since `since` when that is known.
    case inCall(since: Date?)
    case reconnecting
}

/// A call in session as a stage (web `CallSplitContainer`): who it is with,
/// everyone's tile, and the controls. It keeps the Night ground in both
/// appearances, as a cinema does: pictures read best on dark.
///
/// Pure: everything it shows is passed in, so it renders without a session.
struct CallStageView: View {
    /// The peer, or the room.
    let title: String
    let subtitle: String
    let status: CallStageStatus
    let tiles: [CallStageTile]
    var presentation = CallStagePresentation()
    /// Who the speaker view shows large.
    var promotedSpeaker: String?
    /// How much of the window the stage has, for the buttons that change it.
    var size = CallSurface.Size.split
    var isMicrophoneOn = false
    var isCameraOn = false
    /// Whether our hand is up; `nil` in a call with no hands to raise, a 1:1.
    var isHandRaised: Bool?
    /// Something the person should know about their own devices.
    var notice: String?
    /// Where the tiles get their pictures and send a pin.
    let tileSource: CallTileSource
    /// Off in a snapshot, which has nobody to speak to.
    var announcesChanges = true
    let actions: CallStageActions

    var body: some View {
        VStack(spacing: 0) {
            CallStageHeader(title: title, subtitle: subtitle, status: status)
            if let notice {
                Text(notice)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.warningText)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Theme.Spacing.l)
                    .padding(.bottom, Theme.Spacing.s)
            }
            CallTileArea(
                tiles: tiles, presentation: presentation, promoted: promotedSpeaker, source: tileSource
            )
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.top, WaddleTokens.Spacing.s1Half)
            .padding(.bottom, Theme.Spacing.m)
            CallControlBar(
                isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn, hasMedia: status != .connecting,
                isHandRaised: isHandRaised, viewMode: presentation.viewMode,
                isSelfViewHidden: presentation.isSelfViewHidden, size: size, actions: actions
            )
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.top, Theme.Spacing.m)
            .padding(.bottom, Theme.Spacing.l)
        }
        .background(Theme.Colors.seaDeep)
        .environment(\.huddleGround, Theme.Colors.seaDeep)
        .environment(\.colorScheme, .dark)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call with \(title)"))
        .onChange(of: announcement, initial: true) { _, text in
            if announcesChanges { AccessibilityNotification.Announcement(text).post() }
        }
    }

    /// What VoiceOver says when the status changes; never the ticking clock.
    private var announcement: String {
        switch status {
        case .connecting: "Connecting"
        case .inCall: "Call connected"
        case .reconnecting: "Reconnecting"
        }
    }
}

/// The top of the stage (web `CallStageHeader`): a dot that turns green once
/// the call is live, the title over its subtitle, and the status or clock.
private struct CallStageHeader: View {
    let title: String
    let subtitle: String
    let status: CallStageStatus

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
        if case .inCall = status { true } else { false }
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
        switch status {
        case .connecting:
            Text("Connecting…").foregroundStyle(Theme.Colors.fgMuted)
        case .reconnecting:
            Text("Reconnecting…").foregroundStyle(Theme.Colors.warningText)
        case .inCall(nil):
            Text("In call").foregroundStyle(Theme.Colors.fgMuted)
        case .inCall(let since?):
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
