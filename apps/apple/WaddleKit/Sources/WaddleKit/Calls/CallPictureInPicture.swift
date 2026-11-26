/// A call floating above the other windows (web `call-picture-in-picture.ts`).
public enum CallPictureInPicture {
    /// Whose picture floats (web `selectCallPictureInPictureTile`): the
    /// pinned tile, else whoever is speaking, else someone else's camera,
    /// else our own, else a shared screen. Only a tile with a picture
    /// floats; `nil` when nobody has one, and then there is nothing to offer.
    ///
    /// - Parameter activeSpeakers: the identities speaking now.
    public static func tile(
        in tiles: [CallStageTile], presentation: CallStagePresentation, activeSpeakers: [String]
    ) -> CallStageTile? {
        let pictures = tiles.filter { $0.hasVideo && presentation.shows($0) }
        let cameras = pictures.filter { $0.source == .camera }
        let speaking = Set(activeSpeakers)
        return pictures.first { $0.id == presentation.pinnedTileID }
            ?? cameras.first { speaking.contains($0.identity) }
            ?? cameras.first { !$0.isSelf }
            ?? cameras.first
            ?? pictures.first
    }
}
