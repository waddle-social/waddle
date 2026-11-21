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
    public var hasUsableLink: Bool {
        link.trimmed.isEmpty || URL(string: link.trimmed)?.scheme != nil
    }
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

    /// No link, or one that opens in a browser.
    public var hasUsableLink: Bool {
        cleaned.link.isEmpty || webLink != nil
    }

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
}
