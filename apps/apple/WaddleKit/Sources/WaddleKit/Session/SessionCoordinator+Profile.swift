import Foundation

extension SessionCoordinator {
    /// Fetches `jid`'s XEP-0084 avatar once per session.
    public func loadAvatarIfNeeded(_ jid: BareJID) {
        guard avatars.beginFetchIfNeeded(jid) else { return }
        let port = self.port
        Task { [weak self] in
            let image = await port.fetchAvatar(of: jid)
            self?.avatars.finishFetch(jid, image: image)
        }
    }

    public func publishAvatar(_ image: AvatarImage) async throws {
        try await port.publishAvatar(image)
        avatars.finishFetch(account.jid, image: image)
    }

    public func removeAvatar() async throws {
        try await port.removeAvatar()
        avatars.finishFetch(account.jid, image: nil)
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
