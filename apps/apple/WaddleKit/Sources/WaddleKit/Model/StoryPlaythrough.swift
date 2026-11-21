import Foundation

/// The stories open in the viewer: which one shows, how far its video has
/// played, and whether that video is paused or muted.
public struct StoryPlaythrough: Hashable, Sendable {
    /// What the viewer does after a move.
    public enum Step: Hashable, Sendable {
        case show
        /// Past the last story.
        case close
    }

    public private(set) var stories: [Story]
    public private(set) var index: Int
    /// How far the shown story's video has played, from 0 to 1.
    public var progress: Double = 0
    public var isPaused = false
    /// Kept from one story to the next.
    public var isMuted = false

    /// Opens at `start` among `stories`, or on its own when it is not one
    /// of them.
    public init(start: Story, among stories: [Story] = []) {
        if let index = stories.firstIndex(where: { $0.id == start.id }) {
            self.stories = stories
            self.index = index
        } else {
            self.stories = [start]
            index = 0
        }
    }

    public var current: Story { stories[index] }

    /// Moves by `offset`, to the next story's start. Going back from the
    /// first story stays on it.
    public mutating func step(_ offset: Int) -> Step {
        let next = index + offset
        guard next < stories.count else { return .close }
        guard next >= 0, next != index else { return .show }
        index = next
        progress = 0
        isPaused = false
        return .show
    }

    /// The video of `storyID` played to its end: on to the next story, as
    /// a tap would. The end of a story already left changes nothing.
    public mutating func videoEnded(_ storyID: String) -> Step {
        guard storyID == current.id else { return .show }
        return step(1)
    }
}
