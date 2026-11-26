/// A call floating above the other windows (web `call-picture-in-picture.ts`).
public enum CallPictureInPicture {
    /// Whose picture floats (web `selectCallPictureInPictureTile`): the
    /// pinned tile, else the speaker, else someone else's camera,
    /// else our own, else a shared screen. Only a tile with a picture
    /// floats; `nil` when nobody has one, and then there is nothing to offer.
    ///
    /// - Parameter speaker: who the speaker view shows large (`SpeakerPromotion`):
    ///   whoever spoke last, held through their pauses. The web takes whoever
    ///   speaks this instant; here a change of picture builds a renderer on
    ///   the main thread (#2625), so it must not flicker with every pause.
    public static func tile(
        in tiles: [CallStageTile], presentation: CallStagePresentation, speaker: String?
    ) -> CallStageTile? {
        let pictures = tiles.filter { $0.hasVideo && presentation.shows($0) }
        let cameras = pictures.filter { $0.source == .camera }
        return pictures.first { $0.id == presentation.pinnedTileID }
            ?? cameras.first { $0.identity == speaker }
            ?? cameras.first { !$0.isSelf }
            ?? cameras.first
            ?? pictures.first
    }
}
