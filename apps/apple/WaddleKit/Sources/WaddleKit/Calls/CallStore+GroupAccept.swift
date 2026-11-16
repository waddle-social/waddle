extension CallStore {
    /// A matching group sid is consumed even when its room or sender is wrong.
    func fulfillGroupAccept(_ event: CallEvent) -> Bool {
        guard case .sessionAccept(let join, _) = event.kind else { return false }
        if let pending = groupControl.accepts[event.sid] {
            guard event.from.bare == groupControl.mixer, JID(parsing: join.room)?.bare == pending.room else { return true }
            groupControl.accepts.removeValue(forKey: event.sid)
            pending.waiter.resolve(join)
            return true
        }
        guard let room = groupControl.abandonedAccepts[event.sid] else { return false }
        guard event.from.bare == groupControl.mixer, JID(parsing: join.room)?.bare == room else { return true }
        groupControl.abandonedAccepts.removeValue(forKey: event.sid)
        let epoch = generation
        enqueue { [weak self] in
            guard let self else { return }
            // A first late accept can postdate teardown's terminate. Queue its
            // cleanup behind any outstanding leave, even if that leave is held.
            await self.terminateAbandonedGroup(in: room, sid: event.sid, epoch: epoch)
        }
        return true
    }

    /// A teardown's leave may still be in flight when an accepted setup resumes.
    func terminateAbandonedGroup(in room: BareJID, sid: String, epoch: Int) async {
        _ = await groupControl.serializePresence { [self] in
            guard generation == epoch else { return nil }
            await terminateGroup(in: room, sid: sid, epoch: epoch)
            return true
        }
    }

    func cancelGroupAccept(_ sid: String) {
        guard let pending = groupControl.accepts.removeValue(forKey: sid) else { return }
        groupControl.abandonAccept(sid, room: pending.room)
        pending.waiter.cancel()
    }
}
