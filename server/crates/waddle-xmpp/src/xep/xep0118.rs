//! XEP-0118: User Tune.
//!
//! Publishes what the user is listening to over PEP
//! (`http://jabber.org/protocol/tune`). Every field is optional; publishing
//! an empty `<tune/>` clears the current tune.

use minidom::Element;
use thiserror::Error;

pub const NS_TUNE: &str = "http://jabber.org/protocol/tune";
pub const PEP_NODE_TUNE: &str = "http://jabber.org/protocol/tune";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tune {
    /// Song artist.
    pub artist: Option<String>,
    /// Track length in seconds.
    pub length: Option<u32>,
    /// User rating 1–10.
    pub rating: Option<u8>,
    /// Album title.
    pub source: Option<String>,
    /// Track title.
    pub title: Option<String>,
    /// Track number on the album.
    pub track: Option<String>,
    /// URI for more information about the tune.
    pub uri: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TuneError {
    #[error("expected <tune/> in '{NS_TUNE}'")]
    WrongElement,
    #[error("invalid length: {0}")]
    InvalidLength(String),
    #[error("invalid rating: {0}")]
    InvalidRating(String),
}

impl Tune {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_artist(mut self, artist: impl Into<String>) -> Self {
        self.artist = Some(artist.into());
        self
    }
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
    pub fn with_length(mut self, length: u32) -> Self {
        self.length = Some(length);
        self
    }
    pub fn with_rating(mut self, rating: u8) -> Self {
        self.rating = Some(rating);
        self
    }
    pub fn with_track(mut self, track: impl Into<String>) -> Self {
        self.track = Some(track.into());
        self
    }
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    /// True when the tune carries no identifying info — equivalent to a
    /// retraction.
    pub fn is_empty(&self) -> bool {
        self.artist.is_none()
            && self.length.is_none()
            && self.rating.is_none()
            && self.source.is_none()
            && self.title.is_none()
            && self.track.is_none()
            && self.uri.is_none()
    }
}

fn text_child(name: &str, value: &str) -> Element {
    Element::builder(name, NS_TUNE).append(value).build()
}

pub fn build_tune_element(tune: &Tune) -> Element {
    let mut builder = Element::builder("tune", NS_TUNE);
    if let Some(artist) = &tune.artist {
        builder = builder.append(text_child("artist", artist));
    }
    if let Some(length) = tune.length {
        builder = builder.append(text_child("length", &length.to_string()));
    }
    if let Some(rating) = tune.rating {
        builder = builder.append(text_child("rating", &rating.to_string()));
    }
    if let Some(source) = &tune.source {
        builder = builder.append(text_child("source", source));
    }
    if let Some(title) = &tune.title {
        builder = builder.append(text_child("title", title));
    }
    if let Some(track) = &tune.track {
        builder = builder.append(text_child("track", track));
    }
    if let Some(uri) = &tune.uri {
        builder = builder.append(text_child("uri", uri));
    }
    builder.build()
}

/// Build the empty `<tune/>` retraction element.
pub fn build_tune_retraction() -> Element {
    Element::builder("tune", NS_TUNE).build()
}

pub fn parse_tune_element(elem: &Element) -> Result<Tune, TuneError> {
    if !elem.is("tune", NS_TUNE) {
        return Err(TuneError::WrongElement);
    }
    let mut tune = Tune::new();
    for child in elem.children() {
        if child.ns() != NS_TUNE {
            continue;
        }
        let text = child.text();
        match child.name() {
            "artist" => tune.artist = Some(text),
            "title" => tune.title = Some(text),
            "source" => tune.source = Some(text),
            "track" => tune.track = Some(text),
            "uri" => tune.uri = Some(text),
            "length" => {
                let n: u32 = text
                    .trim()
                    .parse()
                    .map_err(|_| TuneError::InvalidLength(text.clone()))?;
                tune.length = Some(n);
            }
            "rating" => {
                let n: u8 = text
                    .trim()
                    .parse()
                    .map_err(|_| TuneError::InvalidRating(text.clone()))?;
                if !(1..=10).contains(&n) {
                    return Err(TuneError::InvalidRating(text));
                }
                tune.rating = Some(n);
            }
            _ => {}
        }
    }
    Ok(tune)
}

pub fn is_tune_element(elem: &Element) -> bool {
    elem.is("tune", NS_TUNE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip_full() {
        let tune = Tune::new()
            .with_artist("The Beatles")
            .with_title("Come Together")
            .with_source("Abbey Road")
            .with_track("1")
            .with_length(259)
            .with_rating(9)
            .with_uri("https://example.com/track");
        let elem = build_tune_element(&tune);
        let parsed = parse_tune_element(&elem).unwrap();
        assert_eq!(parsed, tune);
    }

    #[test]
    fn test_empty_tune_is_retraction() {
        let tune = Tune::new();
        assert!(tune.is_empty());
        let elem = build_tune_element(&tune);
        let parsed = parse_tune_element(&elem).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_retraction_builder_shape() {
        let elem = build_tune_retraction();
        assert!(elem.is("tune", NS_TUNE));
        assert_eq!(elem.children().count(), 0);
    }

    #[test]
    fn test_invalid_length_error() {
        let elem = Element::builder("tune", NS_TUNE)
            .append(
                Element::builder("length", NS_TUNE)
                    .append("not-a-number")
                    .build(),
            )
            .build();
        assert!(matches!(
            parse_tune_element(&elem),
            Err(TuneError::InvalidLength(_))
        ));
    }

    #[test]
    fn test_rating_out_of_range() {
        let elem = Element::builder("tune", NS_TUNE)
            .append(Element::builder("rating", NS_TUNE).append("11").build())
            .build();
        assert!(matches!(
            parse_tune_element(&elem),
            Err(TuneError::InvalidRating(_))
        ));
    }

    #[test]
    fn test_wrong_namespace() {
        let elem = Element::builder("tune", "other").build();
        assert_eq!(parse_tune_element(&elem), Err(TuneError::WrongElement));
    }

    #[test]
    fn test_is_tune_element() {
        assert!(is_tune_element(&Element::builder("tune", NS_TUNE).build()));
        assert!(!is_tune_element(&Element::builder("mood", NS_TUNE).build()));
    }
}
