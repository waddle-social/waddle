import Foundation

/// The stories open in the viewer: which one shows, and whether their
/// videos are muted. Whether a video plays is its player's to say.
public struct StoryPlaythrough: Hashable, Sendable {
    /// What a move did.
    public enum Outcome: Hashable, Sendable {
        case moved
        /// Back from the first story, or the end of a video already left.
        case unchanged
        /// On from the last story: the viewer closes.
        case finished
    }

    public private(set) var stories: [Story]
    public private(set) var index: Int
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
    public var isLast: Bool { index == stories.count - 1 }

    /// Moves by `offset`. Going back from the first story stays on it.
    public mutating func move(by offset: Int) -> Outcome {
        let next = index + offset
        guard next < stories.count else { return .finished }
        guard next >= 0, next != index else { return .unchanged }
        index = next
        return .moved
    }

    /// The video of `storyID` played to its end: on to the next story, as
    /// a tap would.
    public mutating func videoEnded(_ storyID: String) -> Outcome {
        guard storyID == current.id else { return .unchanged }
        return move(by: 1)
    }

    /// How full the bar of the story at `position` is, from 0 to 1: full
    /// for those before the one shown, empty after it. The shown story's
    /// is how much of its video has `played`, or full with no video.
    public func fill(at position: Int, played: Double?) -> Double {
        if position < index { return 1 }
        if position > index { return 0 }
        return min(1, max(0, played ?? 1))
    }
}

/// How a story's video may sound.
public enum StorySound: Hashable, Sendable {
    /// Mixed with other apps' audio, and quiet under the silent switch.
    case mixed
    /// Silent while a call rings or is on, and the call's audio session is
    /// left alone.
    case silencedByCall

    public init(during call: CallState) {
        switch call {
        case .idle, .ended: self = .mixed
        case .incoming, .outgoing, .active, .joiningGroup, .inGroup: self = .silencedByCall
        }
    }
}
