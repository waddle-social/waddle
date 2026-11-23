import Foundation

/// XEP-0108 activity: a general category, an optional specific one inside
/// it, and an optional note.
public struct UserActivity: Hashable, Sendable {
    /// One of the twelve general activities (`working`, `relaxing`, …).
    public var general: String
    /// A specific activity (`coding`, `reading`, …).
    public var specific: String?
    public var text: String?

    public init(general: String, specific: String? = nil, text: String? = nil) {
        self.general = general
        self.specific = specific
        self.text = text
    }

    /// The activity a device publishes by itself for as long as it is in a
    /// call (ADR-010). It belongs to that call, not to the person.
    public var showsACall: Bool {
        general == "talking" && (specific == "on_the_phone" || specific == "on_video_phone")
    }
}

/// XEP-0118 tune. An empty field is one that is not shared.
public struct UserTune: Hashable, Sendable {
    public var title: String
    public var artist: String
    /// The collection the tune is from (`<source/>`).
    public var album: String
    /// Where to hear it (`<uri/>`).
    public var link: String

    public init(title: String = "", artist: String = "", album: String = "", link: String = "") {
        self.title = title
        self.artist = artist
        self.album = album
        self.link = link
    }

    /// The tune without surrounding white space.
    public var cleaned: UserTune {
        UserTune(title: title.trimmed, artist: artist.trimmed, album: album.trimmed, link: link.trimmed)
    }

    public var isEmpty: Bool { cleaned == UserTune() }

    /// No link, or a full address in any scheme: a tune may point into a
    /// music app.
    public var hasUsableLink: Bool { link.trimmed.isEmptyOrFullAddress }
}

/// What an account shares about the moment over PEP.
public struct SharedStatus: Hashable, Sendable {
    public var mood: UserMood?
    public var activity: UserActivity?
    public var tune: UserTune?

    public init(mood: UserMood? = nil, activity: UserActivity? = nil, tune: UserTune? = nil) {
        self.mood = mood
        self.activity = activity
        self.tune = tune
    }

    public var isEmpty: Bool { self == SharedStatus() }

    /// What to show of someone else's status. The mark of a call is shown
    /// only while they are online: a device that goes away without ending
    /// its call leaves the mark behind (ADR-010). A tune with nothing in
    /// it says the music stopped (XEP-0118 §3.2).
    public func shown(isOnline: Bool) -> SharedStatus {
        var shown = self
        if !isOnline, activity?.showsACall == true { shown.activity = nil }
        if tune?.isEmpty == true { shown.tune = nil }
        return shown
    }

    /// What to show after looking again while this is shown. The core
    /// answers a request that failed as nothing shared, so an answer with
    /// nothing at all cannot be told from a lookup that failed and leaves
    /// this; any other answer replaces it.
    public func lookedUpAgain(_ later: SharedStatus) -> SharedStatus {
        later.isEmpty ? self : later
    }

    /// What to show after looking again for the end of a call nothing will
    /// notify (`SessionCoordinator.hearsActivity(of:)`): mood and tune as
    /// `lookedUpAgain`, and the activity found, even none. A request that
    /// failed then hides the call until the next look.
    public func callCheckedAgain(_ later: SharedStatus) -> SharedStatus {
        var next = lookedUpAgain(later)
        next.activity = later.activity
        return next
    }
}

/// The XEP-0292 details of an account. An empty field is one that is not
/// published.
public struct ProfileDetails: Hashable, Sendable {
    public var fullName: String
    public var nickname: String
    public var pronouns: String
    public var bio: String
    public var link: String
    /// Address of the account's picture, written by whoever set the
    /// picture. Carried along so saving the text does not drop it.
    public var photoURI: String?

    public init(
        fullName: String = "", nickname: String = "", pronouns: String = "",
        bio: String = "", link: String = "", photoURI: String? = nil
    ) {
        self.fullName = fullName
        self.nickname = nickname
        self.pronouns = pronouns
        self.bio = bio
        self.link = link
        self.photoURI = photoURI
    }

    /// The details without surrounding white space, and with `https://`
    /// put in front of a link typed without a scheme.
    public var cleaned: ProfileDetails {
        var link = link.trimmed
        if !link.isEmpty, !link.contains(":") { link = "https://" + link }
        return ProfileDetails(
            fullName: fullName.trimmed, nickname: nickname.trimmed, pronouns: pronouns.trimmed,
            bio: bio.trimmed, link: link, photoURI: photoURI
        )
    }

    /// Nothing a person would read: a picture address alone does not count.
    public var hasNoText: Bool {
        var text = cleaned
        text.photoURI = nil
        return text == ProfileDetails()
    }

    /// No link, or a full address. Another app may have stored one that
    /// is not a web link, and that must not stand in the way of a save.
    public var hasUsableLink: Bool { cleaned.link.isEmptyOrFullAddress }

    /// The link when it is safe to open: `http` or `https` with a host.
    /// Details come from other people, so nothing else is ever opened.
    public var webLink: URL? {
        guard let url = URL(string: cleaned.link), let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https", url.host()?.isEmpty == false
        else { return nil }
        return url
    }
}

private extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }

    var isEmptyOrFullAddress: Bool {
        isEmpty || (!contains(where: \.isWhitespace) && URL(string: self)?.scheme != nil)
    }
}
