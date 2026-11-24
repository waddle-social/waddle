import SwiftUI
import WaddleKit

/// Who is in the call (web `CallParticipantsPanel` in the call dock): a row
/// per device with whether it is speaking, its hand, microphone and camera,
/// and how loud we play it. Nobody has a volume for themselves.
///
/// Pure: everything it shows is passed in.
struct CallParticipantsPanel: View {
    let rows: [CallParticipantRow]
    let setVolume: @MainActor (_ identity: String, _ gain: Double) -> Void
    let resetVolumes: @MainActor () -> Void
    let close: @MainActor () -> Void

    static let width: CGFloat = 320

    var body: some View {
        VStack(spacing: 0) {
            header
            Theme.Colors.borderSoft.frame(height: WaddleTokens.BorderWidth.hair)
            ScrollView {
                VStack(spacing: Theme.Spacing.xxs) {
                    ForEach(rows) { row in
                        CallParticipantRowView(row: row) { setVolume(row.id, $0) }
                    }
                }
                .padding(WaddleTokens.Spacing.s1Half)
            }
            Theme.Colors.borderSoft.frame(height: WaddleTokens.BorderWidth.hair)
            HStack {
                Button("Reset all", action: resetVolumes)
                    .buttonStyle(.huddle(size: .small))
                    .disabled(CallParticipantRows.changedVolumes(in: rows).isEmpty)
                    .help("Play everyone at their own volume")
                Spacer()
            }
            .padding(Theme.Spacing.m)
        }
        .background(Theme.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .strokeBorder(Theme.Colors.borderSoft, lineWidth: WaddleTokens.BorderWidth.hair)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Participants"))
    }

    private var header: some View {
        HStack(spacing: Theme.Spacing.s) {
            Text("Participants")
                .huddleText(Theme.Typography.subtitle.sized(WaddleTokens.FontSize.sm))
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
            Text("\(rows.count)")
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.xs))
                .monospacedDigit()
                .foregroundStyle(Theme.Colors.fgMuted)
                .padding(.horizontal, WaddleTokens.Spacing.s1Half)
                .background(Capsule().fill(Theme.Colors.surface2))
                .accessibilityLabel(Text(rows.count == 1 ? "1 person" : "\(rows.count) people"))
            Spacer()
            Button(action: close) {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .frame(width: Metrics.close, height: Metrics.close)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Close participants"))
            .help("Close participants")
        }
        .padding(.leading, Theme.Spacing.m)
        .padding(.trailing, Theme.Spacing.s)
        .frame(height: Metrics.header)
    }

    private enum Metrics {
        static let header: CGFloat = 48
        static let close: CGFloat = 28
    }
}

/// One device: avatar, name and a dot while it speaks, what it has on, and its volume under the name.
private struct CallParticipantRowView: View {
    let row: CallParticipantRow
    let setVolume: @MainActor (Double) -> Void
    @State private var isHovered = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(spacing: WaddleTokens.Spacing.s2Half) {
                CallPersonAvatar(
                    name: row.label, jid: JID(parsing: row.id).flatMap { $0.bare.localpart == nil ? nil : $0.bare },
                    size: Metrics.avatar
                )
                Text(row.label)
                    .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.sm))
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                if row.isSpeaking {
                    Circle().fill(Theme.Colors.live).frame(width: Theme.Size.liveDot, height: Theme.Size.liveDot)
                }
                Spacer(minLength: Theme.Spacing.s)
                devices
            }
            // Read as one: "linus, speaking, microphone on, camera off".
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(row.spokenLabel))
            if let volume = row.volume {
                CallVolumeSlider(name: row.label, gain: volume, set: setVolume)
                    .padding(.leading, Metrics.avatar + WaddleTokens.Spacing.s2Half)
            }
        }
        .padding(.horizontal, WaddleTokens.Spacing.s2Half)
        .padding(.vertical, Theme.Spacing.s)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous).fill(isHovered ? Theme.State.hover : .clear))
        .onHover { isHovered = $0 }
    }

    @ViewBuilder
    private var devices: some View {
        HStack(spacing: WaddleTokens.Spacing.s1Half) {
            if row.isHandRaised {
                Image(systemName: "hand.raised").foregroundStyle(Theme.Colors.goldText)
            }
            if let devices = row.devices {
                Image(systemName: devices.isMicrophoneOn ? "mic" : "mic.slash")
                    .foregroundStyle(devices.isMicrophoneOn ? Theme.Colors.fgMuted : Theme.Colors.dangerText)
                Image(systemName: devices.isCameraOn ? "video" : "video.slash")
            }
        }
        .font(.system(size: Metrics.glyph, weight: .medium))
        .foregroundStyle(Theme.Colors.fgMuted)
    }

    private enum Metrics {
        static let avatar: CGFloat = 28
        static let glyph: CGFloat = 13
    }
}

/// How loud we play one person, from silent to twice their own volume,
/// with a tick at their own volume that a drag settles on (`CallVolume.gain`).
private struct CallVolumeSlider: View {
    let name: String
    let gain: Double
    let set: @MainActor (Double) -> Void

    var body: some View {
        let percent = (gain * 100).rounded()
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: gain == 0 ? "speaker.slash" : "speaker.wave.2")
                .font(.system(size: 12, weight: .medium))
                .frame(width: Metrics.glyph)
                .accessibilityHidden(true)
            Slider(
                value: Binding(
                    get: { percent },
                    set: { set(CallVolume.gain(forPercent: $0.rounded(), currentGain: gain)) }
                ),
                in: 0...200
            )
            .controlSize(.small)
            .tint(Theme.Colors.action)
            .background {
                // Their own volume, at the middle of the track.
                Theme.Colors.borderStrong.frame(width: WaddleTokens.BorderWidth.hair, height: Metrics.tick)
            }
            .accessibilityLabel(Text("Volume of \(name)"))
            .accessibilityValue(Text(gain == 0 ? "Muted" : "\(Int(percent)) percent"))
            Text("\(Int(percent))%")
                .huddleText(Theme.Typography.caption)
                .monospacedDigit()
                .lineLimit(1)
                .fixedSize()
                .frame(width: Metrics.readout, alignment: .trailing)
                .accessibilityHidden(true)
        }
        .foregroundStyle(Theme.Colors.fgMuted)
    }

    private enum Metrics {
        static let glyph: CGFloat = 16
        static let tick: CGFloat = 14
        static let readout: CGFloat = 40
    }
}
