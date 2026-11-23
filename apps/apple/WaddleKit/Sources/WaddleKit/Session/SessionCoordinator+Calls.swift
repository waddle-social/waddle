extension SessionCoordinator {
    func stopCalls() async {
        if calls.state != .idle {
            await waitForCallTeardown { [calls] in await calls.hangUp() }
        }
        // Fence suspended sends before disconnect can suspend too; a late
        // terminate must never send a finish on the next signed-in session.
        calls.clear()
    }

    /// A transport request may ignore cancellation. Race it against a
    /// deadline so sign-out can continue even if the request is still waiting.
    func waitForCallTeardown(_ work: @escaping @MainActor () async -> Void) async {
        let (completed, continuation) = AsyncStream<Void>.makeStream()
        let teardown = Task {
            await work()
            continuation.yield(())
            continuation.finish()
        }
        let deadline = Task { [callTeardownTimeout] in
            do { try await Task.sleep(for: callTeardownTimeout) } catch { return }
            continuation.yield(())
            continuation.finish()
        }
        var iterator = completed.makeAsyncIterator()
        _ = await iterator.next()
        teardown.cancel()
        deadline.cancel()
        continuation.finish()
    }
}

public extension SessionCoordinator {
    /// A room's call is joined by announcing it in the room, so only an
    /// occupant can: the room has let us in, we are online, and the one call
    /// slot is free.
    func canJoinGroupCall(in room: BareJID) -> Bool {
        connection == .online && !isStopped && !calls.state.isLive
            && !isFenced(room)
            && !directory.hasLeft(room)
            && presence.joinedRooms.contains(room)
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
