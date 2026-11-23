public enum SpeakerPromotion {
    /// Rank speakers for the next handoff. Equal levels keep the roster order.
    public static func activeLoudestFirst(
        _ participants: [(identity: String, isSpeaking: Bool, audioLevel: Float)]
    ) -> [String] {
        // `sorted` may reorder equal elements, so the roster position breaks ties.
        participants.enumerated().filter(\.element.isSpeaking)
            .sorted { ($1.element.audioLevel, $0.offset) < ($0.element.audioLevel, $1.offset) }
            .map(\.element.identity)
    }

    /// Hold the current speaker through silence and co-speakers to keep the
    /// large tile stable. On a handoff, the loudest current speaker wins.
    public static func advance(_ promoted: String?, activeLoudestFirst: [String]) -> String? {
        if activeLoudestFirst.isEmpty { return promoted }
        if let promoted, activeLoudestFirst.contains(promoted) { return promoted }
        return activeLoudestFirst.first
    }
}
