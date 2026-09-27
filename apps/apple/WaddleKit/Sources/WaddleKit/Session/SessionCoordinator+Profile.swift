import Foundation

extension SessionCoordinator {
    /// A row showing `jid` rendered: look up its XEP-0084 avatar unless a
    /// current one is held. Offline rows wait for the next session, which
    /// marks every avatar stale.
    public func loadAvatarIfNeeded(_ jid: BareJID) {
        guard connection == .online else { return }
        avatars.request(jid)
    }

    /// The real JID behind a row's author, for its avatar. A room nick is
    /// never turned into a JID: only the room's word counts. That is the
    /// JID stamped on the row (archived `muc#user` item, or the occupant
    /// when an undelayed message arrived). An archive page without real
    /// JIDs falls back to the occupant, or the last JID seen, for the nick;
    /// a live row never does, since a delayed one may predate a handover.
    public func authorJID(of item: TimelineItem) -> BareJID? {
        guard item.conversation.isRoom else { return item.from?.bare }
        if let jid = item.message.authorRealJID { return jid }
        if !item.message.isLive, let nick = item.from?.resource,
           let jid = presence.realJID(ofNick: nick, in: item.conversation.jid) {
            return jid
        }
        return item.isMine ? account.jid : nil
    }

    public func publishAvatar(_ image: AvatarImage) async throws {
        try await port.publishAvatar(image)
        avatars.set(account.jid, image: image)
    }

    public func removeAvatar() async throws {
        try await port.removeAvatar()
        avatars.set(account.jid, image: nil)
    }

    /// RFC 6121 availability with an optional status message.
    public func setAvailability(_ availability: Availability, statusText: String?) async {
        let text = statusText?.trimmingCharacters(in: .whitespacesAndNewlines)
        status.availability = availability
        status.statusText = (text?.isEmpty ?? true) ? nil : text
        guard connection == .online else { return }
        await port.sendPresence(availability, status: status.statusText)
    }

    /// XEP-0107 mood; nil retracts it.
    public func setMood(_ mood: UserMood?) async throws {
        if let mood {
            try await port.publishMood(mood)
        } else {
            try await port.retractMood()
        }
        status.mood = mood
    }

    public func loadOwnMood() async {
        status.mood = try? await port.fetchMood(of: account.jid)
    }

    /// XEP-0363 slot for an attachment the platform layer then uploads.
    public func uploadSlot(filename: String, size: Int, mediaType: String) async -> UploadSlot? {
        guard connection == .online else { return nil }
        return await port.requestUploadSlot(filename: filename, size: size, mediaType: mediaType)
    }

    public func registerPush(deviceToken: String, environment: PushEnvironment, appID: String) async -> PushRegistration? {
        guard connection == .online else { return nil }
        return await port.registerPush(deviceToken: deviceToken, environment: environment, appID: appID)
    }

    public func disablePush(_ registration: PushRegistration) async -> Bool {
        await port.disablePush(registration)
    }

    public func enablePush(_ registration: PushRegistration) async -> Bool {
        guard connection == .online else { return false }
        return await port.enablePush(registration)
    }
}
