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
    @ObservationIgnored private let cameraChoiceChanged: @MainActor () async -> Void
    @ObservationIgnored private nonisolated(unsafe) var deviceObservers: [any NSObjectProtocol] = []

    init(defaults: UserDefaults, cameraChoiceChanged: @escaping @MainActor () async -> Void) {
        self.defaults = defaults
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
        let cameras = ((try? await CameraCapturer.captureDevices()) ?? []).filter(Self.isOffered).map {
            CallDevice(id: $0.uniqueID, name: $0.localizedName)
        }
        var found = CallDevices(cameras: CallDeviceList(devices: cameras, preferred: devicePreferences[.camera]))
        #if os(macOS)
        // The system's own choice is our "System default" row, not a device of its own.
        func listed(_ all: [AudioDevice]) -> [CallDevice] {
            all.filter { !$0.isDefault }.map { CallDevice(id: $0.deviceId, name: $0.name) }
        }
        let audio = AudioManager.shared
        found.microphones = CallDeviceList(devices: listed(audio.inputDevices), preferred: devicePreferences[.microphone])
        found.speakers = CallDeviceList(devices: listed(audio.outputDevices), preferred: devicePreferences[.speaker])
        // A chosen device that is gone gives way to the system's, and takes over again when it is back.
        let input = audio.inputDevices.first { $0.deviceId == found.microphones.chosen?.id } ?? audio.defaultInputDevice
        if audio.inputDevice.deviceId != input.deviceId { audio.inputDevice = input }
        let output = audio.outputDevices.first { $0.deviceId == found.speakers.chosen?.id } ?? audio.defaultOutputDevice
        if audio.outputDevice.deviceId != output.deviceId { audio.outputDevice = output }
        #endif
        if devices != found { devices = found }
    }

    func chooseDevice(_ kind: CallDeviceKind, id: String?) async {
        devicePreferences[kind] = id
        defaults.set(devicePreferences.stored, forKey: Self.devicePreferencesKey)
        await refreshDevices()
        if kind == .camera { await cameraChoiceChanged() }
    }

    /// On iPhone and iPad the system lists every lens, and the combinations
    /// of them, as cameras of their own: one front and one back is the choice a person has.
    private static func isOffered(_ camera: AVCaptureDevice) -> Bool {
        #if os(iOS)
        camera.deviceType == .builtInWideAngleCamera || camera.deviceType == .external
        #else
        true
        #endif
    }
}

enum SystemMicrophoneMode {
    static var active: CallMicrophoneMode {
        switch AVCaptureDevice.activeMicrophoneMode {
        case .standard: .standard
        case .voiceIsolation: .voiceIsolation
        case .wideSpectrum: .wideSpectrum
        @unknown default: .unknown
        }
    }

    @MainActor static func showPicker() {
        AVCaptureDevice.showSystemUserInterface(.microphoneModes)
    }

    @MainActor static func observe(_ update: (CallMicrophoneMode) -> Void) async {
        let (changes, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let observer = Observer(continuation: continuation)
        let key = #keyPath(AVCaptureDevice.activeMicrophoneMode)
        // The active mode belongs to the class, so observe the Objective-C class object.
        let device: AnyObject = AVCaptureDevice.self
        device.addObserver(observer, forKeyPath: key, options: .initial, context: nil)
        defer { device.removeObserver(observer, forKeyPath: key) }
        for await _ in changes {
            guard !Task.isCancelled else { break }
            update(active)
        }
    }

    private final class Observer: NSObject {
        let continuation: AsyncStream<Void>.Continuation

        init(continuation: AsyncStream<Void>.Continuation) {
            self.continuation = continuation
        }

        override func observeValue(
            forKeyPath keyPath: String?, of object: Any?, change: [NSKeyValueChangeKey: Any]?, context: UnsafeMutableRawPointer?
        ) {
            continuation.yield(())
        }
    }
}
