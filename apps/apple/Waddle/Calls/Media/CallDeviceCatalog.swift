import AVFoundation
import Foundation
import LiveKit
import Observation
import WaddleKit

@MainActor
@Observable
final class CallDeviceCatalog {
    private(set) var devices = CallDevices()
    /// What the person chose, kept on this device (web keeps it in local storage).
    @ObservationIgnored private var devicePreferences: CallDevicePreferences
    @ObservationIgnored private let defaults: UserDefaults
    private static let devicePreferencesKey = "callDevicePreferences"
    @ObservationIgnored private let capture: any CallCaptureSources
    @ObservationIgnored private let cameraChoiceChanged: @MainActor () async -> Void
    @ObservationIgnored private nonisolated(unsafe) var deviceObservers: [any NSObjectProtocol] = []

    init(defaults: UserDefaults, capture: any CallCaptureSources, cameraChoiceChanged: @escaping @MainActor () async -> Void) {
        self.defaults = defaults
        self.capture = capture
        self.cameraChoiceChanged = cameraChoiceChanged
        devicePreferences = CallDevicePreferences(stored: defaults.dictionary(forKey: Self.devicePreferencesKey))
        #if os(macOS)
        // A headset plugged in mid-call is offered at once, and one the person chose comes back into use.
        AudioManager.shared.onDeviceUpdate = { [weak self] _ in
            Task { @MainActor in await self?.refreshDevices() }
        }
        #endif
        // A camera plugged in or pulled out: the list follows, and so does a camera that is on.
        for name in [AVCaptureDevice.wasConnectedNotification, AVCaptureDevice.wasDisconnectedNotification] {
            deviceObservers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in
                    await self?.refreshDevices()
                    await self?.cameraChoiceChanged()
                }
            })
        }
    }

    deinit {
        deviceObservers.forEach(NotificationCenter.default.removeObserver)
    }

    func refreshDevices() async {
        _ = await refreshCameras()
        #if os(macOS)
        refreshAudioDevices()
        #endif
    }

    /// Re-read the relevant list for each start, including when a missing device
    /// is checked again. A failed enumeration leaves availability unknown.
    func captureAvailability(for kind: AVMediaType) async -> Bool? {
        if kind == .video { return await refreshCameras() }
        #if os(macOS)
        refreshAudioDevices()
        return !devices.microphones.devices.isEmpty
        #else
        // iOS routes audio through the system, so call settings list no microphones.
        return nil
        #endif
    }

    private func refreshCameras() async -> Bool? {
        guard let cameras = try? await capture.listCameras() else { return nil }
        let found = CallDeviceList(devices: cameras, preferred: devicePreferences[.camera])
        if devices.cameras != found { devices.cameras = found }
        return !found.devices.isEmpty
    }

    #if os(macOS)
    private func refreshAudioDevices() {
        // The system's own choice is our "System default" row, not a device of its own.
        func listed(_ all: [AudioDevice]) -> [CallDevice] {
            all.filter { !$0.isDefault }.map { CallDevice(id: $0.deviceId, name: $0.name) }
        }
        let audio = AudioManager.shared
        let inputs = audio.inputDevices
        var found = devices
        found.microphones = CallDeviceList(devices: listed(inputs), preferred: devicePreferences[.microphone])
        found.speakers = CallDeviceList(devices: listed(audio.outputDevices), preferred: devicePreferences[.speaker])
        // A chosen device that is gone gives way to the system's, and takes over again when it is back.
        let input = inputs.first { $0.deviceId == found.microphones.chosen?.id } ?? audio.defaultInputDevice
        if audio.inputDevice.deviceId != input.deviceId { audio.inputDevice = input }
        let output = audio.outputDevices.first { $0.deviceId == found.speakers.chosen?.id } ?? audio.defaultOutputDevice
        if audio.outputDevice.deviceId != output.deviceId { audio.outputDevice = output }
        if devices != found { devices = found }
    }
    #endif

    func chooseDevice(_ kind: CallDeviceKind, id: String?) async {
        devicePreferences[kind] = id
        defaults.set(devicePreferences.stored, forKey: Self.devicePreferencesKey)
        await refreshDevices()
        if kind == .camera { await cameraChoiceChanged() }
    }
}
