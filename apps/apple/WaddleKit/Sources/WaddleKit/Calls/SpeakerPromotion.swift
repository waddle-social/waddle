public enum SpeakerPromotion {
    /// Hold the current speaker through silence and co-speakers to keep the
    /// large tile stable. On a handoff, the loudest current speaker wins.
    public static func advance(_ promoted: String?, activeLoudestFirst: [String]) -> String? {
        if activeLoudestFirst.isEmpty { return promoted }
        if let promoted, activeLoudestFirst.contains(promoted) { return promoted }
        return activeLoudestFirst.first
    }
}
