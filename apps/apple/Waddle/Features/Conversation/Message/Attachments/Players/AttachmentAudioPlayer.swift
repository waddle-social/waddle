import SwiftUI
import WaddleKit

/// Shows inline audio playback, scrubbing and mute controls.
struct AttachmentAudioPlayer: View {
    let file: SharedFile
    let playback: InlineMediaPlayback
    let togglePlayback: () -> Void
    @State private var scrubbing: Double?

    var body: some View {
        MessageMediaCard {
            HStack(spacing: Theme.Spacing.xs) {
                HuddleIconButton(playLabel, symbol: playback.isPlaying ? "pause.fill" : "play.fill", action: togglePlayback)
                    .foregroundStyle(Theme.Colors.actionText)
                VStack(spacing: Theme.Spacing.xxs) {
                    Slider(value: position, in: 0...max(0.001, playback.duration ?? 1), onEditingChanged: editPosition)
                        .disabled(playback.player == nil || playback.duration == nil)
                        .tint(Theme.Colors.actionText)
                        .accessibilityLabel("Playback position")
                        .accessibilityValue(spokenTimes)
                    HStack {
                        Text(clock(displayedElapsed))
                        Spacer()
                        Text(playback.duration.map(clock) ?? "–:––")
                    }
                    .huddleText(Theme.Typography.caption)
                    .monospacedDigit()
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .accessibilityHidden(true)
                }
                AttachmentMuteButton(playback: playback)
            }
            AttachmentMediaCaption(file: file, kind: .audio)
                .accessibilityElement(children: .combine)
                .accessibilityValue(spokenTimes)
                .accessibilityAction(named: Text(playLabel), togglePlayback)
            if playback.didFail {
                Text("Couldn't play audio")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                Link("Open attachment", destination: file.url)
            }
        }
    }

    private var playLabel: String {
        if playback.didFail { return "Retry audio" }
        return playback.isPlaying ? "Pause audio" : "Play audio"
    }

    private var position: Binding<Double> {
        Binding {
            min(playback.duration ?? 1, displayedElapsed)
        } set: { value in
            if scrubbing == nil {
                playback.seek(to: value)
            } else {
                scrubbing = value
            }
        }
    }

    private var displayedElapsed: Double { max(0, scrubbing ?? playback.elapsed) }

    private func editPosition(_ editing: Bool) {
        if editing {
            scrubbing = position.wrappedValue
        } else if let scrubbing {
            playback.seek(to: scrubbing)
            self.scrubbing = nil
        }
    }

    private func clock(_ seconds: Double) -> String {
        Duration.seconds(seconds).formatted(.time(pattern: seconds >= 3600 ? .hourMinuteSecond : .minuteSecond))
    }

    private var spokenTimes: Text {
        let format = Duration.UnitsFormatStyle(allowedUnits: [.hours, .minutes, .seconds], width: .wide)
        let elapsed = Duration.seconds(displayedElapsed).formatted(format)
        if let duration = playback.duration {
            return Text("\(elapsed) elapsed, \(Duration.seconds(duration).formatted(format)) total")
        }
        return Text("\(elapsed) elapsed, duration unavailable")
    }
}
