import Foundation

/// A microphone, speaker or camera a call can use.
public struct CallDevice: Equatable, Identifiable, Sendable {
    /// The system's identifier, stable while the device exists and across launches.
    public let id: String
    public let name: String

    public init(id: String, name: String) {
        self.id = id
        self.name = name
    }
}

public enum CallDeviceKind: String, CaseIterable, Sendable {
    case microphone, speaker, camera
}

/// The devices of one kind, and which of them the person chose (web `resolveCallDevicePreference`).
public struct CallDeviceList: Equatable, Sendable {
    public let devices: [CallDevice]
    /// The device the person asked for, whether or not it is connected.
    public let preferred: String?

    public init(devices: [CallDevice] = [], preferred: String? = nil) {
        self.devices = devices
        self.preferred = preferred
    }

    /// The chosen device while it is connected; `nil` follows the system's own choice.
    public var chosen: CallDevice? {
        preferred.flatMap { id in devices.first { $0.id == id } }
    }

    /// The person chose a device that is not here now. The system's choice
    /// stands in, and theirs is kept for when the device comes back.
    public var isChoiceMissing: Bool { preferred != nil && chosen == nil }
}

public struct CallDevices: Equatable, Sendable {
    public var microphones: CallDeviceList
    public var speakers: CallDeviceList
    public var cameras: CallDeviceList

    public init(
        microphones: CallDeviceList = CallDeviceList(), speakers: CallDeviceList = CallDeviceList(),
        cameras: CallDeviceList = CallDeviceList()
    ) {
        self.microphones = microphones
        self.speakers = speakers
        self.cameras = cameras
    }

    public subscript(kind: CallDeviceKind) -> CallDeviceList {
        switch kind {
        case .microphone: microphones
        case .speaker: speakers
        case .camera: cameras
        }
    }
}

/// Which devices the person chose for calls, kept on this device (web `device-prefs`).
public struct CallDevicePreferences: Equatable, Sendable {
    private var chosen: [CallDeviceKind: String] = [:]

    public init() { /* Everything follows the system. */ }

    public subscript(kind: CallDeviceKind) -> String? {
        get { chosen[kind] }
        set { chosen[kind] = newValue.flatMap { $0.isEmpty ? nil : $0 } }
    }

    /// As stored: a kind's name to its device. Anything else in there is ignored.
    public init(stored: [String: Any]?) {
        for kind in CallDeviceKind.allCases {
            self[kind] = stored?[kind.rawValue] as? String
        }
    }

    public var stored: [String: String] {
        Dictionary(uniqueKeysWithValues: chosen.map { ($0.key.rawValue, $0.value) })
    }
}
