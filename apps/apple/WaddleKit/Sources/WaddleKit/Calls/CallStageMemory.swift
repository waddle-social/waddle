/// What the person chose on a call's stage, kept for that call: it follows
/// them away from the conversation and back, and a new call starts fresh.
public struct CallStageMemory: Equatable, Sendable {
    public private(set) var sid: String?
    public var presentation = CallStagePresentation()
    /// How much of the window the stage takes. A split stage has no room for the participants panel.
    public var size = CallSurface.Size.defaultSize {
        didSet { if size == .split { showsParticipants = false } }
    }
    public private(set) var showsParticipants = false
    /// Who the speaker view shows large.
    public private(set) var promotedSpeaker: String?

    public init() { /* A stage nobody has touched. */ }

    public mutating func follow(call sid: String) {
        guard sid != self.sid else { return }
        self = Self()
        self.sid = sid
    }

    public mutating func togglePin(_ tile: CallStageTile.ID) {
        presentation.pinnedTileID = presentation.pinnedTileID == tile ? nil : tile
    }

    /// Opening the panel from a split stage expands it, as on the web.
    public mutating func toggleParticipants() {
        if showsParticipants {
            showsParticipants = false
        } else {
            size = size == .split ? .expanded : size
            showsParticipants = true
        }
    }

    /// Call when the tiles or the self-view choice change. While the roster
    /// is unavailable, as during a reconnect, the tiles are stand-ins and a pin outlasts them.
    public mutating func tilesChanged(_ tiles: [CallStageTile], rosterIsAvailable: Bool) {
        if rosterIsAvailable { presentation.reconcile(with: tiles) }
    }

    public mutating func speakersChanged(_ activeLoudestFirst: [String]) {
        promotedSpeaker = SpeakerPromotion.advance(promotedSpeaker, activeLoudestFirst: activeLoudestFirst)
    }
}
