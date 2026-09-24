import Foundation

/// One pasted item to attach.
enum ComposerPasteContent {
    /// A copied file.
    case file(URL)
    /// Image bytes as the pasteboard offered them.
    case image(Data, mediaType: String?, fileExtension: String?)
}

/// What a paste should do: insert text, or attach files and images.
enum ComposerPasteResult {
    case text(String?)
    case attachments([ComposerPasteContent])
}
