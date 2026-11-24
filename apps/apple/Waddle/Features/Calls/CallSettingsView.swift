import SwiftUI
import WaddleKit

/// The call's settings (web `CallSettingsDialog`): which microphone,
/// speaker and camera calls use. A choice holds for this call and the next ones.
///
/// Pure: the devices are passed in and a choice is sent out.
struct CallSettingsView: View {
    let devices: CallDevices
    /// The kinds this device lets an app choose; the others follow the system.
    var kinds: [CallDeviceKind] = CallDeviceKind.allCases
    var audioProcessing = CallAudioProcessing()
    var setAudioProcessing: @MainActor (CallAudioProcessing) -> Void = { _ in /* a sheet without it */ }
    /// The call's video streams as measured; `nil` where nothing measures, which hides the section.
    var videoStats: [CallVideoStat]?
    let choose: @MainActor (CallDeviceKind, String?) -> Void
    let done: @MainActor () -> Void

    static let width: CGFloat = 420

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Call settings")
                .huddleText(Theme.Typography.subtitle.sized(WaddleTokens.FontSize.md))
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
                .padding(Theme.Spacing.l)
            VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                ForEach(kinds, id: \.self) { kind in
                    section(kind, devices[kind])
                    // The web lists it under the microphone; where the microphone cannot be chosen it stands alone.
                    if kind == .microphone { processing }
                }
                if !kinds.contains(.microphone) { processing }
                if let videoStats { video(videoStats) }
            }
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.bottom, Theme.Spacing.l)
            Theme.Colors.borderSoft.frame(height: WaddleTokens.BorderWidth.hair)
            HStack {
                Spacer()
                Button("Done", action: done)
                    .buttonStyle(.huddle(.primary, size: .small))
                    .keyboardShortcut(.defaultAction)
            }
            .padding(Theme.Spacing.m)
        }
        // A Mac's sheet takes its size from its content; a phone's is as wide as the phone.
        #if os(macOS)
        .frame(width: Self.width)
        .onExitCommand(perform: done)
        #else
        .frame(maxWidth: Self.width)
        #endif
        .background(Theme.Colors.bg)
    }

    private func section(_ kind: CallDeviceKind, _ list: CallDeviceList) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Text(Self.title(kind))
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.sm))
                .foregroundStyle(Theme.Colors.fg)
            if list.devices.isEmpty {
                Text(Self.none(kind))
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                Picker(Self.title(kind), selection: Binding(get: { list.chosen?.id }, set: { choose(kind, $0) })) {
                    Text("System default").tag(String?.none)
                    ForEach(list.devices) { device in
                        Text(device.name).tag(String?.some(device.id))
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                if list.isChoiceMissing {
                    Text(Self.missing(kind))
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.warningText)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    /// How each video stream of the call is doing (web "Video" statistics).
    private func video(_ stats: [CallVideoStat]) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            Text("Video")
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.sm))
                .foregroundStyle(Theme.Colors.fg)
            if stats.isEmpty {
                Text("No video right now. Turn on your camera or share your screen to see its resolution, bitrate and packet loss.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            // A big room has many cameras: the list scrolls, and the sheet keeps its height and its Done.
            if stats.count > Self.videoRowsShown {
                ScrollView { videoRows(stats) }.frame(height: Self.videoRowsHeight)
            } else {
                videoRows(stats)
            }
        }
    }

    private static let videoRowsShown = 4
    private static let videoRowsHeight: CGFloat = 176

    private func videoRows(_ stats: [CallVideoStat]) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            ForEach(stats) { stat in
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    HStack {
                        Text(stat.title).foregroundStyle(Theme.Colors.fg)
                        Spacer(minLength: Theme.Spacing.s)
                        Text("\(stat.resolution) · \(stat.framesPerSecond)").monospacedDigit()
                    }
                    Text("\(stat.bitrate) · loss \(stat.loss) · \(stat.roundTrip) · \(stat.codec) · \(stat.path)")
                        .monospacedDigit()
                        .fixedSize(horizontal: false, vertical: true)
                }
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(Text(
                    "\(stat.title): \(stat.resolution), \(stat.framesPerSecond), \(stat.bitrate), loss \(stat.loss), round trip \(stat.roundTrip), \(stat.codec), \(stat.path)"
                ))
            }
        }
    }

    /// What is done to our voice before it is sent (web "Requested audio processing").
    private var processing: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Text("Audio processing")
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.sm))
                .foregroundStyle(Theme.Colors.fg)
            toggle("Noise suppression", \.noiseSuppression)
            toggle("Echo cancellation", \.echoCancellation)
            toggle("Automatic gain control", \.autoGainControl)
        }
    }

    private func toggle(_ title: String, _ setting: WritableKeyPath<CallAudioProcessing, Bool>) -> some View {
        Toggle(title, isOn: Binding(
            get: { audioProcessing[keyPath: setting] },
            set: { isOn in
                var next = audioProcessing
                next[keyPath: setting] = isOn
                setAudioProcessing(next)
            }
        ))
        .toggleStyle(.switch)
        .controlSize(.small)
        .huddleText(Theme.Typography.body.sized(WaddleTokens.FontSize.sm))
        .foregroundStyle(Theme.Colors.fg)
    }

    private static func title(_ kind: CallDeviceKind) -> String {
        switch kind {
        case .microphone: "Microphone"
        case .speaker: "Speaker"
        case .camera: "Camera"
        }
    }

    private static func none(_ kind: CallDeviceKind) -> String {
        switch kind {
        case .microphone: "No microphones found. Waddle may not be allowed to use the microphone."
        case .speaker: "No speakers found."
        case .camera: "No cameras found. Waddle may not be allowed to use the camera."
        }
    }

    private static func missing(_ kind: CallDeviceKind) -> String {
        switch kind {
        case .microphone: "The microphone you chose is not connected. The system’s is used until it is back."
        case .speaker: "The speaker you chose is not connected. The system’s is used until it is back."
        case .camera: "The camera you chose is not connected. The system’s is used until it is back."
        }
    }
}

extension CallMediaControls {
    /// The kinds of device a person can choose here: only a Mac lets an app pick the microphone and speaker.
    var choosableDeviceKinds: [CallDeviceKind] {
        #if os(macOS)
        CallDeviceKind.allCases
        #else
        [.camera]
        #endif
    }
}

/// Presents the call's settings over whatever asked for them, reading and
/// writing the session's media.
struct CallSettingsSheet: ViewModifier {
    @Binding var isPresented: Bool
    @Environment(\.callMediaControls) private var media

    func body(content: Content) -> some View {
        content.sheet(isPresented: $isPresented) {
            if let media { CallSettingsContent(media: media) { isPresented = false } }
        }
    }
}

/// The settings of the session's call. The figures renewed every second
/// are this view's own state, so a tick redraws the sheet and never the
/// stage or the sidebar that presented it.
private struct CallSettingsContent: View {
    let media: any CallMediaControls
    let done: @MainActor () -> Void
    @State private var tracker = CallVideoStatsTracker()
    @State private var videoStats: [CallVideoStat] = []

    var body: some View {
        CallSettingsView(
            devices: media.devices, kinds: media.choosableDeviceKinds, audioProcessing: media.audioProcessing,
            setAudioProcessing: { processing in Task { await media.setAudioProcessing(processing) } },
            videoStats: videoStats,
            choose: { kind, id in Task { await media.chooseDevice(kind, id: id) } },
            done: done
        )
        // What is plugged in may have changed since the call began.
        .task { await media.refreshDevices() }
        // Measured only while someone is looking, once a second as the web does.
        .task {
            tracker = CallVideoStatsTracker()
            await media.setMeasuresVideo(true)
            while !Task.isCancelled {
                videoStats = tracker.update(with: await media.videoSamples())
                try? await Task.sleep(for: .seconds(1))
            }
            await media.setMeasuresVideo(false)
        }
    }
}

extension View {
    func callSettings(isPresented: Binding<Bool>) -> some View {
        modifier(CallSettingsSheet(isPresented: isPresented))
    }
}
