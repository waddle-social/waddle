//! What a profile holds and how it reads, without widgets: the XEP-0292
//! vCard fields, and the XEP-0107 mood, XEP-0108 activity and XEP-0118
//! tune an account shares over PEP.

use url::Url;
use waddle_xmpp_client_ffi::{
    WaddleActivity, WaddleMood, WaddlePepProfile, WaddleTune, WaddleVCard4,
};

/// The XEP-0292 details of an account. An empty field is one that is not
/// published.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    pub full_name: String,
    pub nickname: String,
    pub pronouns: String,
    pub bio: String,
    pub link: String,
    /// Address of the account's picture, written by whoever set it.
    /// Carried along so saving the text does not drop it.
    pub photo_uri: Option<String>,
}

impl Details {
    pub fn from_wire(vcard: Option<WaddleVCard4>) -> Self {
        let Some(vcard) = vcard else {
            return Self::default();
        };
        Self {
            full_name: vcard.full_name.unwrap_or_default(),
            nickname: vcard.nickname.unwrap_or_default(),
            pronouns: vcard.pronouns.unwrap_or_default(),
            bio: vcard.note.unwrap_or_default(),
            link: vcard.url.unwrap_or_default(),
            photo_uri: vcard.photo_uri,
        }
    }

    /// The vCard to publish: trimmed, with blank fields left out.
    pub fn to_wire(&self) -> WaddleVCard4 {
        let present = |text: &str| {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        WaddleVCard4 {
            full_name: present(&self.full_name),
            nickname: present(&self.nickname),
            pronouns: present(&self.pronouns),
            note: present(&self.bio),
            url: present(&self.link),
            photo_uri: self.photo_uri.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.to_wire().full_name.is_none()
            && self.to_wire().nickname.is_none()
            && self.to_wire().pronouns.is_none()
            && self.to_wire().note.is_none()
            && self.to_wire().url.is_none()
    }

    /// The name to show: the full name, else the nickname, else `fallback`.
    pub fn display_name(&self, fallback: &str) -> String {
        [&self.full_name, &self.nickname]
            .into_iter()
            .map(|name| name.trim())
            .find(|name| !name.is_empty())
            .unwrap_or(fallback)
            .to_string()
    }

    /// The link when it is safe to open from here: only `http` and
    /// `https`, since a profile is written by someone else.
    pub fn openable_link(&self) -> Option<Url> {
        openable(&self.link)
    }
}

/// `text` as a link that may be opened: an `http` or `https` address.
pub fn openable(text: &str) -> Option<Url> {
    let url = Url::parse(text.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(url)
}

/// "in_love" reads "In love".
pub fn title_of(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// XEP-0107's closed registry of moods, as the core publishes them.
pub const MOODS: [&str; 84] = [
    "afraid",
    "amazed",
    "amorous",
    "angry",
    "annoyed",
    "anxious",
    "aroused",
    "ashamed",
    "bored",
    "brave",
    "calm",
    "cautious",
    "cold",
    "confident",
    "confused",
    "contemplative",
    "contented",
    "cranky",
    "crazy",
    "creative",
    "curious",
    "dejected",
    "depressed",
    "disappointed",
    "disgusted",
    "dismayed",
    "distracted",
    "embarrassed",
    "envious",
    "excited",
    "flirtatious",
    "frustrated",
    "grateful",
    "grieving",
    "grumpy",
    "guilty",
    "happy",
    "hopeful",
    "hot",
    "humbled",
    "humiliated",
    "hungry",
    "hurt",
    "impressed",
    "in_awe",
    "in_love",
    "indignant",
    "interested",
    "intoxicated",
    "invincible",
    "jealous",
    "lonely",
    "lost",
    "lucky",
    "mean",
    "moody",
    "nervous",
    "neutral",
    "offended",
    "outraged",
    "playful",
    "proud",
    "relaxed",
    "relieved",
    "remorseful",
    "restless",
    "sad",
    "sarcastic",
    "satisfied",
    "serious",
    "shocked",
    "shy",
    "sick",
    "sleepy",
    "spontaneous",
    "stressed",
    "strong",
    "surprised",
    "thankful",
    "thirsty",
    "tired",
    "undefined",
    "weak",
    "worried",
];

/// A general activity and the specific ones XEP-0108 defines under it.
pub struct ActivityKind {
    pub general: &'static str,
    pub title: &'static str,
    pub specifics: &'static [&'static str],
}

/// The activities offered in the picker. `talking` is left out:
/// `talking` with `on_the_phone` is how a call in progress is shown to
/// contacts (ADR-010), so picking it by hand would read as a call.
pub const ACTIVITIES: [ActivityKind; 10] = [
    ActivityKind {
        general: "working",
        title: "Working",
        specifics: &["coding", "in_a_meeting", "studying", "writing"],
    },
    ActivityKind {
        general: "relaxing",
        title: "Relaxing",
        specifics: &[
            "fishing",
            "gaming",
            "going_out",
            "partying",
            "reading",
            "rehearsing",
            "shopping",
            "smoking",
            "socializing",
            "sunbathing",
            "watching_a_movie",
            "watching_tv",
        ],
    },
    ActivityKind {
        general: "eating",
        title: "Eating",
        specifics: &[
            "having_a_snack",
            "having_breakfast",
            "having_dinner",
            "having_lunch",
        ],
    },
    ActivityKind {
        general: "drinking",
        title: "Drinking",
        specifics: &["having_a_beer", "having_coffee", "having_tea"],
    },
    ActivityKind {
        general: "exercising",
        title: "Exercising",
        specifics: &[
            "cycling",
            "dancing",
            "hiking",
            "jogging",
            "playing_sports",
            "running",
            "skiing",
            "swimming",
            "working_out",
        ],
    },
    ActivityKind {
        general: "traveling",
        title: "Travelling",
        specifics: &[
            "commuting",
            "cycling",
            "driving",
            "in_a_car",
            "on_a_bus",
            "on_a_plane",
            "on_a_train",
            "on_a_trip",
            "walking",
        ],
    },
    ActivityKind {
        general: "doing_chores",
        title: "Chores",
        specifics: &[
            "buying_groceries",
            "cleaning",
            "cooking",
            "doing_maintenance",
            "doing_the_dishes",
            "doing_the_laundry",
            "gardening",
            "running_an_errand",
            "walking_the_dog",
        ],
    },
    ActivityKind {
        general: "grooming",
        title: "Grooming",
        specifics: &[
            "at_the_spa",
            "brushing_teeth",
            "getting_a_haircut",
            "shaving",
            "taking_a_bath",
            "taking_a_shower",
        ],
    },
    ActivityKind {
        general: "having_appointment",
        title: "Appointment",
        specifics: &[],
    },
    ActivityKind {
        general: "inactive",
        title: "Time off",
        specifics: &[
            "day_off",
            "hanging_out",
            "hiding",
            "on_vacation",
            "praying",
            "scheduled_holiday",
            "sleeping",
            "thinking",
        ],
    },
];

pub fn activity_kind(general: &str) -> Option<&'static ActivityKind> {
    ACTIVITIES.iter().find(|kind| kind.general == general)
}

/// A general activity's title, including ones another client may publish
/// that the picker does not offer.
pub fn general_title(general: &str) -> String {
    match (activity_kind(general), general) {
        (Some(kind), _) => kind.title.to_string(),
        (None, "talking") => "Talking".to_string(),
        (None, "undefined") => "Something else".to_string(),
        (None, other) => title_of(other),
    }
}

pub fn specific_title(specific: &str) -> String {
    match specific {
        "watching_tv" => "Watching TV".to_string(),
        other => title_of(other),
    }
}

/// An activity that marks a call in progress (ADR-010). It belongs to
/// the call, not the person, so a profile editor leaves it alone.
pub fn shows_a_call(activity: &WaddleActivity) -> bool {
    activity.general == "talking"
        && matches!(
            activity.specific.as_deref(),
            Some("on_the_phone" | "on_video_phone")
        )
}

/// "Working · Coding", or the general activity alone; a call reads
/// "In a call".
pub fn describe_activity(activity: &WaddleActivity) -> String {
    if shows_a_call(activity) {
        return "In a call".to_string();
    }
    let general = general_title(&activity.general);
    let mut line = match activity.specific.as_deref() {
        Some(specific) => format!("{general} · {}", specific_title(specific)),
        None => general,
    };
    if let Some(text) = non_blank(activity.text.as_deref()) {
        line.push_str(&format!(" ({text})"));
    }
    line
}

/// "Happy" or "Happy (made my day)".
pub fn describe_mood(mood: &WaddleMood) -> String {
    let kind = title_of(&mood.kind);
    match non_blank(mood.text.as_deref()) {
        Some(text) => format!("{kind} ({text})"),
        None => kind,
    }
}

/// "Title · Artist · Album"; none for a tune with nothing in it, which
/// says the music stopped (XEP-0118 §3.2).
pub fn describe_tune(tune: &WaddleTune) -> Option<String> {
    let parts: Vec<&str> = [
        tune.title.as_deref(),
        tune.artist.as_deref(),
        tune.source.as_deref(),
    ]
    .into_iter()
    .filter_map(non_blank)
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn non_blank(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty())
}

/// What an account shares about the moment.
#[derive(Debug, Clone, Default)]
pub struct Shared {
    pub mood: Option<WaddleMood>,
    pub activity: Option<WaddleActivity>,
    pub tune: Option<WaddleTune>,
}

impl Shared {
    pub fn from_wire(profile: WaddlePepProfile) -> Self {
        Self {
            mood: profile.mood,
            activity: profile.activity,
            tune: profile.tune.filter(|tune| describe_tune(tune).is_some()),
        }
    }

    /// The lines to show: each present item with its heading.
    pub fn lines(&self, online: bool) -> Vec<(&'static str, String)> {
        let mut lines = Vec::new();
        if let Some(mood) = &self.mood {
            lines.push(("Mood", describe_mood(mood)));
        }
        // A device that goes away without ending its call leaves the mark
        // behind, so it is shown only while the person is online.
        if let Some(activity) = self
            .activity
            .as_ref()
            .filter(|activity| online || !shows_a_call(activity))
        {
            lines.push(("Activity", describe_activity(activity)));
        }
        if let Some(tune) = self.tune.as_ref().and_then(describe_tune) {
            lines.push(("Listening to", tune));
        }
        lines
    }
}

/// The tune to publish from the editor's fields, or none when they are
/// all blank (which retracts instead). A link may be any address: a tune
/// can point into a music app.
pub fn tune_from_fields(title: &str, artist: &str, album: &str, link: &str) -> Option<WaddleTune> {
    let field = |text: &str| non_blank(Some(text)).map(str::to_string);
    let tune = WaddleTune {
        artist: field(artist),
        title: field(title),
        source: field(album),
        track: None,
        uri: field(link),
        length_seconds: None,
        rating: None,
    };
    let empty = tune.artist.is_none()
        && tune.title.is_none()
        && tune.source.is_none()
        && tune.uri.is_none();
    (!empty).then_some(tune)
}

/// Whether `text` is blank or a full address in any scheme.
pub fn blank_or_address(text: &str) -> bool {
    let text = text.trim();
    text.is_empty() || Url::parse(text).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(general: &str, specific: Option<&str>, text: Option<&str>) -> WaddleActivity {
        WaddleActivity {
            general: general.to_string(),
            specific: specific.map(str::to_string),
            text: text.map(str::to_string),
        }
    }

    #[test]
    fn saving_trims_fields_and_leaves_blanks_out() {
        let details = Details {
            full_name: "  Ann Ex ".to_string(),
            nickname: "   ".to_string(),
            pronouns: "she/her".to_string(),
            bio: String::new(),
            link: " https://ann.example ".to_string(),
            photo_uri: Some("https://x.test/p.png".to_string()),
        };
        let wire = details.to_wire();
        assert_eq!(wire.full_name.as_deref(), Some("Ann Ex"));
        assert_eq!(wire.nickname, None);
        assert_eq!(wire.pronouns.as_deref(), Some("she/her"));
        assert_eq!(wire.note, None);
        assert_eq!(wire.url.as_deref(), Some("https://ann.example"));
        // The picture address is kept so saving text does not drop it.
        assert_eq!(wire.photo_uri.as_deref(), Some("https://x.test/p.png"));
    }

    #[test]
    fn a_vcard_round_trips_and_a_missing_one_is_empty() {
        assert!(Details::from_wire(None).is_empty());
        let wire = WaddleVCard4 {
            full_name: Some("Ann".to_string()),
            nickname: None,
            pronouns: None,
            note: Some("Hi".to_string()),
            url: None,
            photo_uri: None,
        };
        let details = Details::from_wire(Some(wire));
        assert_eq!(details.bio, "Hi");
        assert!(!details.is_empty());
        assert_eq!(details.to_wire().note.as_deref(), Some("Hi"));
    }

    #[test]
    fn the_display_name_prefers_the_full_name_then_the_nickname() {
        let mut details = Details::default();
        assert_eq!(details.display_name("ann"), "ann");
        details.nickname = "annie".to_string();
        assert_eq!(details.display_name("ann"), "annie");
        details.full_name = "Ann Ex".to_string();
        assert_eq!(details.display_name("ann"), "Ann Ex");
    }

    #[test]
    fn only_http_and_https_links_open() {
        assert!(openable("https://ann.example/me").is_some());
        assert!(openable(" http://ann.example ").is_some());
        for refused in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "xmpp:ann@x.test",
            "ann.example",
            "https://",
            "",
        ] {
            assert!(openable(refused).is_none(), "{refused}");
        }
    }

    #[test]
    fn registry_names_read_as_titles() {
        assert_eq!(title_of("in_love"), "In love");
        assert_eq!(title_of("happy"), "Happy");
        assert_eq!(title_of(""), "");
        assert_eq!(specific_title("watching_tv"), "Watching TV");
        assert_eq!(general_title("inactive"), "Time off");
        assert_eq!(general_title("talking"), "Talking");
    }

    #[test]
    fn the_registries_have_no_duplicates() {
        let mut moods = MOODS.to_vec();
        moods.sort_unstable();
        moods.dedup();
        assert_eq!(moods.len(), MOODS.len());
        for kind in &ACTIVITIES {
            let mut specifics = kind.specifics.to_vec();
            specifics.sort_unstable();
            specifics.dedup();
            assert_eq!(specifics.len(), kind.specifics.len(), "{}", kind.general);
        }
        assert!(activity_kind("talking").is_none());
        assert!(activity_kind("working").is_some());
    }

    #[test]
    fn a_call_is_never_offered_or_overwritten_by_hand() {
        let call = activity("talking", Some("on_video_phone"), None);
        assert!(shows_a_call(&call));
        assert_eq!(describe_activity(&call), "In a call");
        assert!(!shows_a_call(&activity(
            "talking",
            Some("in_real_life"),
            None
        )));
        assert!(!shows_a_call(&activity("working", None, None)));
    }

    #[test]
    fn activities_mood_and_tune_read_as_lines() {
        assert_eq!(
            describe_activity(&activity("working", Some("coding"), Some("on the client"))),
            "Working · Coding (on the client)"
        );
        assert_eq!(describe_activity(&activity("eating", None, None)), "Eating");
        let mood = WaddleMood {
            kind: "in_love".to_string(),
            text: Some("  ".to_string()),
        };
        assert_eq!(describe_mood(&mood), "In love");
        let tune = WaddleTune {
            artist: Some("Artist".to_string()),
            title: Some("Song".to_string()),
            source: None,
            track: None,
            uri: None,
            length_seconds: None,
            rating: None,
        };
        assert_eq!(describe_tune(&tune).as_deref(), Some("Song · Artist"));
        assert_eq!(
            describe_tune(&WaddleTune {
                artist: None,
                title: None,
                ..tune
            }),
            None
        );
    }

    #[test]
    fn a_call_mark_is_shown_only_while_online() {
        let shared = Shared {
            mood: None,
            activity: Some(activity("talking", Some("on_the_phone"), None)),
            tune: None,
        };
        assert_eq!(shared.lines(true).len(), 1);
        assert!(shared.lines(false).is_empty());
        let working = Shared {
            activity: Some(activity("working", None, None)),
            ..Shared::default()
        };
        assert_eq!(working.lines(false).len(), 1);
    }

    #[test]
    fn a_tune_with_nothing_in_it_is_the_music_stopping() {
        let profile = WaddlePepProfile {
            mood: None,
            activity: None,
            tune: Some(WaddleTune {
                artist: None,
                title: None,
                source: None,
                track: None,
                uri: None,
                length_seconds: None,
                rating: None,
            }),
        };
        assert!(Shared::from_wire(profile).lines(true).is_empty());
    }

    #[test]
    fn tune_fields_publish_trimmed_and_all_blank_publishes_nothing() {
        assert!(tune_from_fields(" ", "", "", "  ").is_none());
        let tune = tune_from_fields(" Song ", "Artist", "", "spotify:track:1").expect("tune");
        assert_eq!(tune.title.as_deref(), Some("Song"));
        assert_eq!(tune.source, None);
        assert_eq!(tune.uri.as_deref(), Some("spotify:track:1"));
        assert!(blank_or_address(""));
        assert!(blank_or_address("spotify:track:1"));
        assert!(!blank_or_address("not an address"));
    }
}
