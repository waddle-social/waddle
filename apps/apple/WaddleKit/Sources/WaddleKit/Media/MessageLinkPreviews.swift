import Foundation

/// A direct-video link uses an XEP-0511 card and a separate inline XEP-0447 file.
/// The card takes the matching file so it has one play control.
struct MessageLinkPreviews {
    let previews: [LinkPreview]
    let remainingFiles: [SharedFile]

    init(previews: [LinkPreview], files: [SharedFile]) {
        var consumed: Set<Int> = []
        self.previews = previews.map { preview in
            guard preview.playback == nil else { return preview }
            let originalURL = preview.originalURL.flatMap { URL(string: $0) }
            for index in files.indices where !consumed.contains(index) {
                let file = files[index]
                guard file.url == preview.url || file.url == originalURL,
                      let video = LinkPreviewVideo(file: file)
                else { continue }
                consumed.insert(index)
                return preview.withPlayback(.video(video))
            }
            return preview
        }
        remainingFiles = files.indices.filter { !consumed.contains($0) }.map { files[$0] }
    }
}
