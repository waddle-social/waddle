import LiveKit
import WaddleKit

@MainActor
final class CallRoomAccess {
    /// The room of the call in progress, from the moment its connect starts.
    var room: Room?

    var tracksChanged: @MainActor () -> Void = {}
    var recordRefusal: @MainActor (CallMediaIssue.Kind) -> Void = { _ in }

    func holds(_ room: Room) -> Bool {
        self.room === room
    }
}
