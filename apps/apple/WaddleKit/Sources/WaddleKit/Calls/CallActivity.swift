/// Decides how this device uses the account's single XEP-0108 activity item.
enum CallActivity {
    struct Publication: Equatable {
        let mark: UserActivity
        let previous: UserActivity?
        /// Our own earlier mark this one was written over. Until the write
        /// is confirmed, finding that one published still means it is ours.
        var replaced: UserActivity?
        /// A lost reply leaves us needing to check whether the mark landed.
        var isConfirmed = true
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
        if let owned, let published, published == owned.mark || (!owned.isConfirmed && published == owned.replaced) {
            guard let desired else { return .write(owned.previous, keeping: nil) }
            guard desired != published else { return .keep }
            return .write(desired, keeping: Publication(mark: desired, previous: owned.previous, replaced: published))
        }
        // Leave another device's edit alone if it replaced a confirmed mark.
        if owned?.isConfirmed == true { return .forget }
        guard let desired, published?.showsACall != true else {
            return owned == nil ? .keep : .forget
        }
        return .write(desired, keeping: Publication(mark: desired, previous: published))
    }
}
