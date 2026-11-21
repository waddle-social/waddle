import Foundation

/// How many times each part of the profile has been set from this device.
/// A lookup that began before an edit holds an older value for that part,
/// and must not put it back.
struct ProfileEdits: Equatable {
    var mood = 0
    var activity = 0
    var tune = 0
    var details = 0
}

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
        item.authorJID
    }

    public func publishAvatar(_ image: AvatarImage) async throws {
        let id = try await port.publishAvatar(image)
        avatars.set(account.jid, image: image, id: id)
    }

    public func removeAvatar() async throws {
        try await port.removeAvatar()
        avatars.set(account.jid, image: nil, id: nil)
    }

    /// XEP-0107 mood; nil retracts it.
    public func setMood(_ mood: UserMood?) async throws {
        if let mood {
            try await port.publishMood(mood)
        } else {
            try await port.retractMood()
        }
        status.mood = mood
        profileEdits.mood += 1
    }

    /// XEP-0108 activity; nil retracts it.
    ///
    /// The account has one activity, and a device in a call uses it to show
    /// the call (ADR-010). What is published is looked up first, and while
    /// it shows a call it is left alone: writing over it would end the "in
    /// a call" mark for everyone. A lookup that fails throws, so nothing
    /// is written while what is published is unknown.
    public func setActivity(_ activity: UserActivity?) async throws {
        if let published = try await port.fetchOwnActivity(), published.showsACall {
            status.activity = published
            throw PortError.rejected
        }
        if let activity {
            try await port.publishActivity(activity)
        } else {
            try await port.retractActivity()
        }
        status.activity = activity
        profileEdits.activity += 1
    }

    /// Looks up the activity the account publishes now. A failed lookup
    /// leaves what is shown, and so does one overtaken by an edit here.
    public func refreshOwnActivity() async {
        let before = profileEdits
        guard let published = try? await port.fetchOwnActivity() as UserActivity??,
              profileEdits.activity == before.activity
        else { return }
        status.activity = published
    }

    /// XEP-0118 tune; nil retracts it. A tune with nothing in it means
    /// "stopped" on the wire (§3.2), so it is refused rather than published.
    public func setTune(_ tune: UserTune?) async throws {
        let tune = tune?.cleaned
        if let tune {
            guard !tune.isEmpty, tune.hasUsableLink else { throw PortError.invalidRequest }
            try await port.publishTune(tune)
        } else {
            try await port.retractTune()
        }
        status.tune = tune
        profileEdits.tune += 1
    }

    /// The mood, activity and tune the account shares now. A failed lookup
    /// leaves what is shown. The editors stay usable while this runs, so a
    /// part set here in the meantime keeps its new value.
    public func loadOwnSharedStatus() async {
        let before = profileEdits
        guard let shared = try? await port.fetchSharedStatus(of: account.jid) else { return }
        if profileEdits.mood == before.mood { status.mood = shared.mood }
        if profileEdits.activity == before.activity { status.activity = shared.activity }
        if profileEdits.tune == before.tune { status.tune = shared.tune }
    }

    /// The account's own XEP-0292 details; empty only when it is confirmed
    /// that none are published. A lookup that fails throws: saving replaces
    /// every field, so details that could not be read must not be edited.
    /// Details saved here while the lookup ran are newer than its answer,
    /// and are what is kept and returned.
    @discardableResult
    public func loadOwnDetails() async throws -> ProfileDetails {
        let before = profileEdits
        let details = try await port.fetchOwnProfileDetails() ?? ProfileDetails()
        if let saved = status.details, profileEdits.details != before.details { return saved }
        status.details = details
        return details
    }

    public func saveDetails(_ details: ProfileDetails) async throws {
        let details = details.cleaned
        guard details.hasUsableLink else { throw PortError.invalidRequest }
        try await port.publishProfileDetails(details)
        status.details = details
        profileEdits.details += 1
    }

    /// Someone's details, or nil when they have published none we can read.
    public func details(of jid: BareJID) async -> ProfileDetails? {
        guard let details = try? await port.fetchProfileDetails(of: jid), !details.hasNoText else { return nil }
        return details
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
