/// Someone the media session reports in the call.
public struct CallStageParticipant: Equatable, Sendable {
    /// The full JID the SFU knows this device by.
    public let identity: String
    public let isSelf: Bool
    public let hasCamera: Bool

    public init(identity: String, isSelf: Bool, hasCamera: Bool) {
        self.identity = identity
        self.isSelf = isSelf
        self.hasCamera = hasCamera
    }
}

/// The tiles a call puts on its stage, one per device, as the web's `buildCallTiles` decides them.
///
/// While media is connecting or reconnecting nobody is live, so the stage
/// shows who the call is known to hold; a tile never waits for video.
public enum CallStageTiles {
    /// A 1:1 call: us and the peer. The peer keeps a tile until a device of theirs joins.
    ///
    /// - Parameter peer: the device the session was set up with, which is the identity it joins as.
    /// - Parameter speaking: the accounts shown as speaking.
    public static func direct(
        live: [CallStageParticipant], own: JID, peer: JID, peerName: String, isSelfMuted: Bool = false,
        speaking: Set<BareJID> = []
    ) -> [CallStageTile] {
        var people = live.isEmpty ? [CallStageParticipant(identity: own.description, isSelf: true, hasCamera: false)] : live
        if !people.contains(where: { !$0.isSelf && JID(parsing: $0.identity)?.bare == peer.bare }) {
            people.append(CallStageParticipant(identity: peer.description, isSelf: false, hasCamera: false))
        }
        return people.map { person in
            let account = JID(parsing: person.identity)?.bare
            return CallStageTile(
                identity: person.identity, source: .camera,
                label: person.isSelf ? "You" : account == peer.bare ? peerName : name(of: person.identity),
                isSelf: person.isSelf, isMuted: person.isSelf && isSelfMuted,
                isSpeaking: account.map(speaking.contains) ?? false, hasVideo: person.hasCamera
            )
        }
    }

    /// A room's call. A live device takes its name and badges from the
    /// roster row of its account; before media connects the roster stands in.
    ///
    /// - Parameter rows: `GroupCallRoster.rows`, which already says who is speaking.
    public static func room(
        live: [CallStageParticipant], own: JID, rows: [GroupCallRosterRow], isSelfMuted: Bool = false
    ) -> [CallStageTile] {
        guard !live.isEmpty else {
            return rows.map { row in
                tile(
                    identity: row.isSelf ? own.description : row.jid?.description ?? row.nick,
                    isSelf: row.isSelf, hasCamera: false, row: row, isSelfMuted: isSelfMuted
                )
            }
        }
        return live.map { person in
            let account = JID(parsing: person.identity)?.bare
            let row = account.flatMap { account in rows.first { $0.jid == account } }
            return tile(
                identity: person.identity, isSelf: person.isSelf, hasCamera: person.hasCamera, row: row,
                isSelfMuted: isSelfMuted
            )
        }
    }

    /// Our own microphone is the truth for our tile; the room hears of it a moment later.
    private static func tile(
        identity: String, isSelf: Bool, hasCamera: Bool, row: GroupCallRosterRow?, isSelfMuted: Bool
    ) -> CallStageTile {
        CallStageTile(
            identity: identity, source: .camera, label: isSelf ? "You" : row?.nick ?? name(of: identity),
            isSelf: isSelf, isMuted: isSelf ? isSelfMuted : row?.isMuted ?? false,
            isHandRaised: row?.isHandRaised ?? false, isSpeaking: row?.isSpeaking ?? false, hasVideo: hasCamera
        )
    }

    private static func name(of identity: String) -> String {
        JID(parsing: identity)?.bare.localpart ?? identity
    }
}

public extension CallStageTile {
    /// What VoiceOver says for the tile: the name, then what its badges show.
    func spokenLabel(isPinned: Bool) -> String {
        var parts = [label]
        if isMuted { parts.append("muted") }
        if isHandRaised { parts.append("hand raised") }
        if isSpeaking { parts.append("speaking") }
        if isPinned { parts.append("pinned") }
        return parts.joined(separator: ", ")
    }
}

/// The line under a stage's title, as on the web.
public enum CallStageSubtitle {
    public static func direct(_ media: CallMedia) -> String {
        media.video ? "Video call" : "Voice call"
    }

    public static func room(others: Int) -> String {
        switch others {
        case ...0: "Waiting for others to join…"
        case 1: "1 other in call"
        default: "\(others) others in call"
        }
    }
}
