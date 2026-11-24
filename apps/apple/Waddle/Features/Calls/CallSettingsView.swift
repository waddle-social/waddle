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
    let choose: @MainActor (CallDeviceKind, String?) -> Void
    let done: @MainActor () -> Void

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
                }
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
        .frame(width: 420)
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
            if let media {
                CallSettingsView(devices: media.devices, kinds: media.choosableDeviceKinds) { kind, id in
                    Task { await media.chooseDevice(kind, id: id) }
                } done: {
                    isPresented = false
                }
                // What is plugged in may have changed since the call began.
                .task { await media.refreshDevices() }
            }
        }
    }
}

extension View {
    func callSettings(isPresented: Binding<Bool>) -> some View {
        modifier(CallSettingsSheet(isPresented: isPresented))
    }
}
