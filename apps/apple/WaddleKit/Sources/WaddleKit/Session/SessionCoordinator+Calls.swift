extension SessionCoordinator {
    /// A transport verb may ignore cancellation, so teardown races an
    /// independent deadline instead of awaiting a cancelled child task.
    func stopCalls() async {
        if calls.state != .idle {
            let (completed, continuation) = AsyncStream<Void>.makeStream()
            let teardown = Task { [calls] in
                await calls.hangUp()
                continuation.yield(())
                continuation.finish()
            }
            let deadline = Task {
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
                continuation.yield(())
                continuation.finish()
            }
            var iterator = completed.makeAsyncIterator()
            _ = await iterator.next()
            teardown.cancel()
            deadline.cancel()
            continuation.finish()
        }
        // Fence suspended sends before disconnect can suspend too; a late
        // terminate must never send a finish on the next signed-in session.
        calls.clear()
    }
}

public extension SessionCoordinator {
    /// A room's call is joined by announcing it in the room, so only an
    /// occupant can: the room has let us in, we are online, and the one call
    /// slot is free.
    func canJoinGroupCall(in room: BareJID) -> Bool {
        connection == .online && !calls.state.isLive && presence.joinedRooms.contains(room)
    }

    /// Starts `room`'s call, or joins the one in progress, under the nick
    /// we hold in that room.
    @discardableResult
    func joinGroupCall(in room: BareJID, video: Bool) async -> Bool {
        guard canJoinGroupCall(in: room) else { return false }
        let nick = presence.selfNicks[room] ?? account.nick
        return await calls.joinGroupCall(in: room, media: CallMedia(audio: true, video: video), nick: nick)
    }
}
