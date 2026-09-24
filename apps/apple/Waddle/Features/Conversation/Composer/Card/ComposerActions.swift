import Foundation
import WaddleKit

/// What the composer card's controls do. `ConversationComposer` owns the
/// state; the card only lays the controls out.
struct ComposerActions {
    let submit: () -> Void
    /// Returns true when a suggestion was inserted.
    let acceptSuggestion: (ComposerSuggestionKey) -> Bool
    /// Returns true when an edit or reply was cancelled.
    let cancelContext: () -> Bool
    let format: (ComposerFormat) -> Void
    let requestLink: () -> Void
    let insertEmoji: (String) -> Void
    let startMention: () -> Void
    let startCommand: () -> Void
    let pickPhoto: () -> Void
    let pickFile: () -> Void
    let pickGIF: () -> Void
    let paste: () -> Void
}
