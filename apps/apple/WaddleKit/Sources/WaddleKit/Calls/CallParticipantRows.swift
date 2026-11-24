/// What the media session knows of a device's sound.
public struct CallParticipantAudio: Equatable, Sendable {
    /// The full JID the SFU knows this device by.
    public let identity: String
    public let hasMicrophone: Bool
    /// Playout gain from 0 to 2, with 1 at unity.
    public let volume: Double

    public init(identity: String, hasMicrophone: Bool, volume: Double) {
        self.identity = identity
        self.hasMicrophone = hasMicrophone
        self.volume = volume
    }
}

/// One device in a call's participants panel (web `CallRosterRow`).
public struct CallParticipantRow: Equatable, Identifiable, Sendable {
    public struct Devices: Equatable, Sendable {
        public let isMicrophoneOn: Bool
        public let isCameraOn: Bool
    }

    /// The full JID the SFU knows this device by.
    public let id: String
    public let label: String
    public let isSelf: Bool
    public let isSpeaking: Bool
    public let isHandRaised: Bool
    /// `nil` until the device is in the media session: before that nobody knows.
    public let devices: Devices?
    /// How loud we play them, 1 at unity. `nil` for ourselves and for
    /// a device with no sound to turn up or down yet.
    public let volume: Double?

    /// What VoiceOver says for the row: the name, then its state.
    public var spokenLabel: String {
        var parts = [label]
        if isSpeaking { parts.append("speaking") }
        if isHandRaised { parts.append("hand raised") }
        if let devices {
            parts.append(devices.isMicrophoneOn ? "microphone on" : "microphone off")
            parts.append(devices.isCameraOn ? "camera on" : "camera off")
        }
        return parts.joined(separator: ", ")
    }
}

public enum CallParticipantRows {
    /// A row per device on the stage, ours first, then the stage's order.
    ///
    /// - Parameter tiles: the stage's tiles, which already carry names, hands, mutes and who is speaking.
    /// - Parameter audio: the devices in the media session; empty while it connects or reconnects.
    public static func make(tiles: [CallStageTile], audio: [CallParticipantAudio]) -> [CallParticipantRow] {
        let cameras = tiles.filter { $0.source == .camera }
        return (cameras.filter(\.isSelf) + cameras.filter { !$0.isSelf }).map { tile in
            let sound = audio.first { $0.identity == tile.identity }
            return CallParticipantRow(
                id: tile.identity, label: tile.label, isSelf: tile.isSelf, isSpeaking: tile.isSpeaking,
                isHandRaised: tile.isHandRaised,
                devices: sound.map {
                    // A muted microphone stays published, so the room's word on a mute wins over the track.
                    .init(isMicrophoneOn: tile.isSelf ? !tile.isMuted : $0.hasMicrophone && !tile.isMuted, isCameraOn: tile.hasVideo)
                },
                volume: tile.isSelf ? nil : sound?.volume
            )
        }
    }

    /// Whose volume "Reset all" has to put back.
    public static func changedVolumes(in rows: [CallParticipantRow]) -> [String] {
        rows.filter { $0.volume != nil && $0.volume != 1 }.map(\.id)
    }
}
