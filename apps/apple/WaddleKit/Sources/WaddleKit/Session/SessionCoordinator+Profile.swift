import Foundation

extension SessionCoordinator {
    /// A row showing `jid` rendered: look up its XEP-0084 avatar unless a
    /// current one is held. Offline rows wait for the next session, which
    /// marks every avatar stale.
    public func loadAvatarIfNeeded(_ jid: BareJID) {
        guard connection == .online else { return }
        avatars.request(jid)
    }

    /// The real JID behind a row's author, for its avatar. A room row
    /// resolves only from its stamp (the archived `muc#user` item, our
    /// local echo, or the occupant when an undelayed message arrived):
    /// a nick may have changed hands since, and initials beat a wrong face.
    public func authorJID(of item: TimelineItem) -> BareJID? {
        guard item.conversation.isRoom else { return item.from?.bare }
        return item.message.authorRealJID
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
