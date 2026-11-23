/// Decides how this device uses the account's single XEP-0108 activity item.
enum CallActivity {
    struct Publication: Equatable {
        let mark: UserActivity
        let previous: UserActivity?
    }

    enum Change: Equatable {
        case keep
        case forget
        case write(UserActivity?, keeping: Publication?)
    }

    static func mark(for state: CallState) -> UserActivity? {
        let media: CallMedia
        switch state {
        case .active(let call): media = call.media
        case .inGroup(let call): media = call.media
        case .idle, .incoming, .outgoing, .joiningGroup, .ended: return nil
        }
        // The call's media stays the same when someone turns their camera off.
        return UserActivity(general: "talking", specific: media.video ? "on_video_phone" : "on_the_phone")
    }

    static func change(desired: UserActivity?, published: UserActivity?, owned: Publication?) -> Change {
        if let owned {
            // Another device may have replaced our last write while the call ran.
            guard published == owned.mark else { return .forget }
            guard let desired else { return .write(owned.previous, keeping: nil) }
            guard desired != owned.mark else { return .keep }
            return .write(desired, keeping: Publication(mark: desired, previous: owned.previous))
        }
        guard let desired, published?.showsACall != true else { return .keep }
        return .write(desired, keeping: Publication(mark: desired, previous: published))
    }
}
