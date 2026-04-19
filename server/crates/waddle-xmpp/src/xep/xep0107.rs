//! XEP-0107: User Mood.
//!
//! Publishes the user's current mood over PEP (`http://jabber.org/protocol/mood`).
//! A mood has a canonical keyword chosen from a closed enum plus an optional
//! free-text description. Clients retract by publishing an empty `<mood/>`.
//!
//! ## XML
//!
//! ```xml
//! <item id='current'>
//!   <mood xmlns='http://jabber.org/protocol/mood'>
//!     <happy/>
//!     <text>Having a great day</text>
//!   </mood>
//! </item>
//! ```

use minidom::Element;
use thiserror::Error;

/// PEP / payload namespace for mood.
pub const NS_MOOD: &str = "http://jabber.org/protocol/mood";

/// PEP node for mood publication.
pub const PEP_NODE_MOOD: &str = "http://jabber.org/protocol/mood";

/// Closed set of mood keywords from XEP-0107 §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoodKind {
    Afraid,
    Amazed,
    Amorous,
    Angry,
    Annoyed,
    Anxious,
    Aroused,
    Ashamed,
    Bored,
    Brave,
    Calm,
    Cautious,
    Cold,
    Confident,
    Confused,
    Contemplative,
    Contented,
    Cranky,
    Crazy,
    Creative,
    Curious,
    Dejected,
    Depressed,
    Disappointed,
    Disgusted,
    Dismayed,
    Distracted,
    Embarrassed,
    Envious,
    Excited,
    Flirtatious,
    Frustrated,
    Grateful,
    Grieving,
    Grumpy,
    Guilty,
    Happy,
    Hopeful,
    Hot,
    Humbled,
    Humiliated,
    Hungry,
    Hurt,
    Impressed,
    InAwe,
    InLove,
    Indignant,
    Interested,
    Intoxicated,
    Invincible,
    Jealous,
    Lonely,
    Lost,
    Lucky,
    Mean,
    Moody,
    Nervous,
    Neutral,
    Offended,
    Outraged,
    Playful,
    Proud,
    Relaxed,
    Relieved,
    Remorseful,
    Restless,
    Sad,
    Sarcastic,
    Satisfied,
    Serious,
    Shocked,
    Shy,
    Sick,
    Sleepy,
    Spontaneous,
    Stressed,
    Strong,
    Surprised,
    Thankful,
    Thirsty,
    Tired,
    Undefined,
    Weak,
    Worried,
}

impl MoodKind {
    /// The element name as it appears on the wire.
    pub fn as_element_name(&self) -> &'static str {
        match self {
            MoodKind::Afraid => "afraid",
            MoodKind::Amazed => "amazed",
            MoodKind::Amorous => "amorous",
            MoodKind::Angry => "angry",
            MoodKind::Annoyed => "annoyed",
            MoodKind::Anxious => "anxious",
            MoodKind::Aroused => "aroused",
            MoodKind::Ashamed => "ashamed",
            MoodKind::Bored => "bored",
            MoodKind::Brave => "brave",
            MoodKind::Calm => "calm",
            MoodKind::Cautious => "cautious",
            MoodKind::Cold => "cold",
            MoodKind::Confident => "confident",
            MoodKind::Confused => "confused",
            MoodKind::Contemplative => "contemplative",
            MoodKind::Contented => "contented",
            MoodKind::Cranky => "cranky",
            MoodKind::Crazy => "crazy",
            MoodKind::Creative => "creative",
            MoodKind::Curious => "curious",
            MoodKind::Dejected => "dejected",
            MoodKind::Depressed => "depressed",
            MoodKind::Disappointed => "disappointed",
            MoodKind::Disgusted => "disgusted",
            MoodKind::Dismayed => "dismayed",
            MoodKind::Distracted => "distracted",
            MoodKind::Embarrassed => "embarrassed",
            MoodKind::Envious => "envious",
            MoodKind::Excited => "excited",
            MoodKind::Flirtatious => "flirtatious",
            MoodKind::Frustrated => "frustrated",
            MoodKind::Grateful => "grateful",
            MoodKind::Grieving => "grieving",
            MoodKind::Grumpy => "grumpy",
            MoodKind::Guilty => "guilty",
            MoodKind::Happy => "happy",
            MoodKind::Hopeful => "hopeful",
            MoodKind::Hot => "hot",
            MoodKind::Humbled => "humbled",
            MoodKind::Humiliated => "humiliated",
            MoodKind::Hungry => "hungry",
            MoodKind::Hurt => "hurt",
            MoodKind::Impressed => "impressed",
            MoodKind::InAwe => "in_awe",
            MoodKind::InLove => "in_love",
            MoodKind::Indignant => "indignant",
            MoodKind::Interested => "interested",
            MoodKind::Intoxicated => "intoxicated",
            MoodKind::Invincible => "invincible",
            MoodKind::Jealous => "jealous",
            MoodKind::Lonely => "lonely",
            MoodKind::Lost => "lost",
            MoodKind::Lucky => "lucky",
            MoodKind::Mean => "mean",
            MoodKind::Moody => "moody",
            MoodKind::Nervous => "nervous",
            MoodKind::Neutral => "neutral",
            MoodKind::Offended => "offended",
            MoodKind::Outraged => "outraged",
            MoodKind::Playful => "playful",
            MoodKind::Proud => "proud",
            MoodKind::Relaxed => "relaxed",
            MoodKind::Relieved => "relieved",
            MoodKind::Remorseful => "remorseful",
            MoodKind::Restless => "restless",
            MoodKind::Sad => "sad",
            MoodKind::Sarcastic => "sarcastic",
            MoodKind::Satisfied => "satisfied",
            MoodKind::Serious => "serious",
            MoodKind::Shocked => "shocked",
            MoodKind::Shy => "shy",
            MoodKind::Sick => "sick",
            MoodKind::Sleepy => "sleepy",
            MoodKind::Spontaneous => "spontaneous",
            MoodKind::Stressed => "stressed",
            MoodKind::Strong => "strong",
            MoodKind::Surprised => "surprised",
            MoodKind::Thankful => "thankful",
            MoodKind::Thirsty => "thirsty",
            MoodKind::Tired => "tired",
            MoodKind::Undefined => "undefined",
            MoodKind::Weak => "weak",
            MoodKind::Worried => "worried",
        }
    }

    pub fn from_element_name(name: &str) -> Option<MoodKind> {
        Some(match name {
            "afraid" => MoodKind::Afraid,
            "amazed" => MoodKind::Amazed,
            "amorous" => MoodKind::Amorous,
            "angry" => MoodKind::Angry,
            "annoyed" => MoodKind::Annoyed,
            "anxious" => MoodKind::Anxious,
            "aroused" => MoodKind::Aroused,
            "ashamed" => MoodKind::Ashamed,
            "bored" => MoodKind::Bored,
            "brave" => MoodKind::Brave,
            "calm" => MoodKind::Calm,
            "cautious" => MoodKind::Cautious,
            "cold" => MoodKind::Cold,
            "confident" => MoodKind::Confident,
            "confused" => MoodKind::Confused,
            "contemplative" => MoodKind::Contemplative,
            "contented" => MoodKind::Contented,
            "cranky" => MoodKind::Cranky,
            "crazy" => MoodKind::Crazy,
            "creative" => MoodKind::Creative,
            "curious" => MoodKind::Curious,
            "dejected" => MoodKind::Dejected,
            "depressed" => MoodKind::Depressed,
            "disappointed" => MoodKind::Disappointed,
            "disgusted" => MoodKind::Disgusted,
            "dismayed" => MoodKind::Dismayed,
            "distracted" => MoodKind::Distracted,
            "embarrassed" => MoodKind::Embarrassed,
            "envious" => MoodKind::Envious,
            "excited" => MoodKind::Excited,
            "flirtatious" => MoodKind::Flirtatious,
            "frustrated" => MoodKind::Frustrated,
            "grateful" => MoodKind::Grateful,
            "grieving" => MoodKind::Grieving,
            "grumpy" => MoodKind::Grumpy,
            "guilty" => MoodKind::Guilty,
            "happy" => MoodKind::Happy,
            "hopeful" => MoodKind::Hopeful,
            "hot" => MoodKind::Hot,
            "humbled" => MoodKind::Humbled,
            "humiliated" => MoodKind::Humiliated,
            "hungry" => MoodKind::Hungry,
            "hurt" => MoodKind::Hurt,
            "impressed" => MoodKind::Impressed,
            "in_awe" => MoodKind::InAwe,
            "in_love" => MoodKind::InLove,
            "indignant" => MoodKind::Indignant,
            "interested" => MoodKind::Interested,
            "intoxicated" => MoodKind::Intoxicated,
            "invincible" => MoodKind::Invincible,
            "jealous" => MoodKind::Jealous,
            "lonely" => MoodKind::Lonely,
            "lost" => MoodKind::Lost,
            "lucky" => MoodKind::Lucky,
            "mean" => MoodKind::Mean,
            "moody" => MoodKind::Moody,
            "nervous" => MoodKind::Nervous,
            "neutral" => MoodKind::Neutral,
            "offended" => MoodKind::Offended,
            "outraged" => MoodKind::Outraged,
            "playful" => MoodKind::Playful,
            "proud" => MoodKind::Proud,
            "relaxed" => MoodKind::Relaxed,
            "relieved" => MoodKind::Relieved,
            "remorseful" => MoodKind::Remorseful,
            "restless" => MoodKind::Restless,
            "sad" => MoodKind::Sad,
            "sarcastic" => MoodKind::Sarcastic,
            "satisfied" => MoodKind::Satisfied,
            "serious" => MoodKind::Serious,
            "shocked" => MoodKind::Shocked,
            "shy" => MoodKind::Shy,
            "sick" => MoodKind::Sick,
            "sleepy" => MoodKind::Sleepy,
            "spontaneous" => MoodKind::Spontaneous,
            "stressed" => MoodKind::Stressed,
            "strong" => MoodKind::Strong,
            "surprised" => MoodKind::Surprised,
            "thankful" => MoodKind::Thankful,
            "thirsty" => MoodKind::Thirsty,
            "tired" => MoodKind::Tired,
            "undefined" => MoodKind::Undefined,
            "weak" => MoodKind::Weak,
            "worried" => MoodKind::Worried,
            _ => return None,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MoodError {
    #[error("expected <mood/> in namespace '{NS_MOOD}'")]
    WrongElement,
    #[error("unknown mood keyword '{0}'")]
    UnknownKind(String),
    #[error("no mood keyword child present")]
    MissingKind,
}

/// A typed Mood value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mood {
    pub kind: MoodKind,
    pub text: Option<String>,
}

impl Mood {
    pub fn new(kind: MoodKind) -> Self {
        Self { kind, text: None }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// Build a `<mood>` element from a typed value.
pub fn build_mood_element(mood: &Mood) -> Element {
    let mut builder = Element::builder("mood", NS_MOOD)
        .append(Element::builder(mood.kind.as_element_name(), NS_MOOD).build());
    if let Some(text) = &mood.text {
        builder = builder.append(
            Element::builder("text", NS_MOOD)
                .append(text.as_str())
                .build(),
        );
    }
    builder.build()
}

/// Build the empty retraction element published to clear a previously
/// announced mood.
pub fn build_mood_retraction() -> Element {
    Element::builder("mood", NS_MOOD).build()
}

/// Parse a `<mood>` element into a typed value. Returns `None` if the element
/// is the empty retraction form (valid but has no content).
pub fn parse_mood_element(elem: &Element) -> Result<Option<Mood>, MoodError> {
    if !elem.is("mood", NS_MOOD) {
        return Err(MoodError::WrongElement);
    }
    // Retraction: no children / only whitespace.
    let mut kind: Option<MoodKind> = None;
    let mut text: Option<String> = None;
    let mut has_any = false;
    for child in elem.children() {
        has_any = true;
        if child.ns() != NS_MOOD {
            continue;
        }
        let name = child.name();
        if name == "text" {
            text = Some(child.text());
        } else if let Some(k) = MoodKind::from_element_name(name) {
            kind = Some(k);
        } else {
            return Err(MoodError::UnknownKind(name.to_string()));
        }
    }
    if !has_any {
        return Ok(None);
    }
    let kind = kind.ok_or(MoodError::MissingKind)?;
    Ok(Some(Mood { kind, text }))
}

/// True when `elem` is a `<mood>` element in `NS_MOOD`.
pub fn is_mood_element(elem: &Element) -> bool {
    elem.is("mood", NS_MOOD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip_with_text() {
        let m = Mood::new(MoodKind::Happy).with_text("Excited for Friday");
        let elem = build_mood_element(&m);
        let parsed = parse_mood_element(&elem).unwrap().unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn test_round_trip_without_text() {
        let m = Mood::new(MoodKind::Calm);
        let elem = build_mood_element(&m);
        let parsed = parse_mood_element(&elem).unwrap().unwrap();
        assert_eq!(parsed, m);
        assert!(parsed.text.is_none());
    }

    #[test]
    fn test_parse_retraction() {
        let elem = build_mood_retraction();
        assert!(parse_mood_element(&elem).unwrap().is_none());
    }

    #[test]
    fn test_unknown_kind_error() {
        let elem = Element::builder("mood", NS_MOOD)
            .append(Element::builder("euphoric", NS_MOOD).build())
            .build();
        assert_eq!(
            parse_mood_element(&elem),
            Err(MoodError::UnknownKind("euphoric".into()))
        );
    }

    #[test]
    fn test_wrong_namespace() {
        let elem = Element::builder("mood", "other").build();
        assert_eq!(parse_mood_element(&elem), Err(MoodError::WrongElement));
    }

    #[test]
    fn test_is_mood_element() {
        let elem = Element::builder("mood", NS_MOOD).build();
        assert!(is_mood_element(&elem));
        let other = Element::builder("mood", "other").build();
        assert!(!is_mood_element(&other));
    }

    #[test]
    fn test_kind_round_trip_names() {
        for k in [
            MoodKind::Happy,
            MoodKind::InAwe,
            MoodKind::InLove,
            MoodKind::Worried,
            MoodKind::Undefined,
        ] {
            assert_eq!(MoodKind::from_element_name(k.as_element_name()), Some(k));
        }
    }
}
