import Foundation

/// The GIF picker, opened from `/giphy` or the `+` menu.
struct ComposerGifSearch: Identifiable {
    let id = UUID()
    /// The search to start with; empty shows trending GIFs.
    let query: String
}

/// The formatting bar's link prompt.
struct ComposerLinkPrompt {
    var isPresented = false
    var text = ""
}
