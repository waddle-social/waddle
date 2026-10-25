//! PEP (Personal Eventing Protocol) user-state publish/subscribe.
//!
//! Covers XEP-0163 (PEP), XEP-0107 (User Mood), XEP-0108 (User Activity),
//! and XEP-0118 (User Tune).

use minidom::Element;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::client::ClientHandle;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::error::ClientResult;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use uuid::Uuid;

/// Protocol token `http://jabber.org/protocol/mood`.
pub const NS_MOOD: &str = "http://jabber.org/protocol/mood";
/// Protocol token `http://jabber.org/protocol/activity`.
pub const NS_ACTIVITY: &str = "http://jabber.org/protocol/activity";
/// XEP-0163 §3 notification filter for the activity node: a roster contact
/// receives a peer's in-call overlay (ADR-010 Phase 3) only if it advertises
/// this in its XEP-0115 caps.
pub const NS_ACTIVITY_NOTIFY: &str = "http://jabber.org/protocol/activity+notify";
/// XEP-0163 §3.4 notification filter for the Waddle status-preference node:
/// the user's own resources receive a manual-status pick (ADR-010 Phase 4)
/// only if they advertise this in their XEP-0115 caps. Pinned to
/// `waddle_xmpp_core::waddle_status_preference::PEP_NODE_WADDLE_STATUS_PREFERENCE`
/// by `caps::tests::status_preference_notify_matches_core_node`.
pub const NS_STATUS_PREFERENCE_NOTIFY: &str = "urn:waddle:status-preference:0+notify";
/// Protocol token `http://jabber.org/protocol/tune`.
pub const NS_TUNE: &str = "http://jabber.org/protocol/tune";
/// Protocol token `http://jabber.org/protocol/pubsub`.
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
/// Protocol token `http://jabber.org/protocol/pubsub#event`.
pub const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
/// XEP-0402 bookmarks PEP node + payload namespace.
pub const NS_BOOKMARKS: &str = "urn:xmpp:bookmarks:1";

const NS_CLIENT: &str = "jabber:client";

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0107 mood token and optional explanatory text published through PEP.
pub struct UserMood {
    /// Selected XEP-0107 mood token.
    pub mood: String,
    /// Human-readable text associated with this value.
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0108 broad and specific activity tokens with optional text.
pub struct UserActivity {
    /// Selected broad XEP-0108 activity token.
    pub activity: String,
    /// More specific activity token nested under the broad activity category.
    pub specific: Option<String>,
    /// Human-readable text associated with this value.
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// XEP-0118 metadata describing the tune currently being played.
pub struct UserTune {
    /// Artist credited for the current tune.
    pub artist: Option<String>,
    /// Human-readable title rendered by the host.
    pub title: Option<String>,
    /// Album or other collection containing the tune.
    pub source: Option<String>,
    /// Duration of the tune in seconds.
    pub length: Option<u32>,
    /// Rating reported for the tune.
    pub rating: Option<u8>,
    /// Track number of the current tune.
    pub track: Option<String>,
    /// URI identifying the referenced resource.
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Supported user mood, activity, and tune publications or retractions.
#[non_exhaustive]
pub enum PepItem {
    /// A published XEP-0107 user mood.
    Mood(UserMood),
    /// A published XEP-0108 user activity.
    Activity(UserActivity),
    /// A published XEP-0118 tune.
    Tune(UserTune),
    /// The user retracted their published mood.
    MoodCleared,
    /// The user retracted their published activity.
    ActivityCleared,
    /// The user retracted their published tune.
    TuneCleared,
}

/// Parse a PEP event from an incoming `<message>` stanza.
#[must_use]
pub fn parse(element: &Element) -> Option<PepItem> {
    if element.name() != "message" {
        return None;
    }

    let event = element.get_child("event", NS_PUBSUB_EVENT)?;
    let items = event.get_child("items", NS_PUBSUB_EVENT)?;
    let node = items.attr("node")?;

    match node {
        NS_MOOD => parse_mood_event(items),
        NS_ACTIVITY => parse_activity_event(items),
        NS_TUNE => parse_tune_event(items),
        _ => None,
    }
}

fn parse_mood_event(items: &Element) -> Option<PepItem> {
    let item = items.get_child("item", NS_PUBSUB_EVENT)?;
    let mood = item.get_child("mood", NS_MOOD)?;
    match parse_mood_payload(mood) {
        Some(payload) => Some(PepItem::Mood(payload)),
        None => Some(PepItem::MoodCleared),
    }
}

fn parse_activity_event(items: &Element) -> Option<PepItem> {
    let item = items.get_child("item", NS_PUBSUB_EVENT)?;
    let activity = item.get_child("activity", NS_ACTIVITY)?;
    match parse_activity_payload(activity) {
        Some(payload) => Some(PepItem::Activity(payload)),
        None => Some(PepItem::ActivityCleared),
    }
}

fn parse_tune_event(items: &Element) -> Option<PepItem> {
    let item = items.get_child("item", NS_PUBSUB_EVENT)?;
    let tune = item.get_child("tune", NS_TUNE)?;
    match parse_tune_payload(tune) {
        Some(payload) => Some(PepItem::Tune(payload)),
        None => Some(PepItem::TuneCleared),
    }
}

fn parse_mood_payload(element: &Element) -> Option<UserMood> {
    let mood = element
        .children()
        .find(|child| child.ns() == NS_MOOD && child.name() != "text")?
        .name()
        .to_string();
    let text = element.get_child("text", NS_MOOD).map(|child| child.text());
    Some(UserMood { mood, text })
}

fn parse_activity_payload(element: &Element) -> Option<UserActivity> {
    let general = element
        .children()
        .find(|child| child.ns() == NS_ACTIVITY && child.name() != "text")?;
    let specific = general
        .children()
        .find(|child| child.ns() == NS_ACTIVITY)
        .map(|child| child.name().to_string());
    let text = element
        .get_child("text", NS_ACTIVITY)
        .map(|child| child.text());
    Some(UserActivity {
        activity: general.name().to_string(),
        specific,
        text,
    })
}

fn parse_tune_payload(element: &Element) -> Option<UserTune> {
    let tune = UserTune {
        artist: element
            .get_child("artist", NS_TUNE)
            .map(|child| child.text()),
        title: element
            .get_child("title", NS_TUNE)
            .map(|child| child.text()),
        source: element
            .get_child("source", NS_TUNE)
            .map(|child| child.text()),
        length: element
            .get_child("length", NS_TUNE)
            .and_then(|child| child.text().parse::<u32>().ok()),
        rating: element
            .get_child("rating", NS_TUNE)
            .and_then(|child| child.text().parse::<u8>().ok()),
        track: element
            .get_child("track", NS_TUNE)
            .map(|child| child.text()),
        uri: element.get_child("uri", NS_TUNE).map(|child| child.text()),
    };
    if tune.artist.is_none()
        && tune.title.is_none()
        && tune.source.is_none()
        && tune.length.is_none()
        && tune.rating.is_none()
        && tune.track.is_none()
        && tune.uri.is_none()
    {
        None
    } else {
        Some(tune)
    }
}

/// Build an XEP-0107 mood element from the selected mood token and optional text.
#[must_use]
pub fn build_mood_element(mood: &str, text: Option<&str>) -> Element {
    let mut builder =
        Element::builder("mood", NS_MOOD).append(Element::builder(mood, NS_MOOD).build());
    if let Some(text) = text {
        builder = builder.append(Element::builder("text", NS_MOOD).append(text).build());
    }
    builder.build()
}

/// Build an empty XEP-0107 mood element that clears the current publication.
#[must_use]
pub fn build_mood_clear_element() -> Element {
    Element::builder("mood", NS_MOOD).build()
}

/// Build an XEP-0108 activity element from a broad activity token and optional text.
#[must_use]
pub fn build_activity_element(activity: &str, text: Option<&str>) -> Element {
    build_activity_element_with_specific(activity, None, text)
}

/// Build an XEP-0108 broad activity with an optional specific subactivity and explanatory text.
#[must_use]
pub fn build_activity_element_with_specific(
    general: &str,
    specific: Option<&str>,
    text: Option<&str>,
) -> Element {
    let mut general_builder = Element::builder(general, NS_ACTIVITY);
    if let Some(specific) = specific {
        general_builder = general_builder.append(Element::builder(specific, NS_ACTIVITY).build());
    }
    let mut builder = Element::builder("activity", NS_ACTIVITY).append(general_builder.build());
    if let Some(text) = text {
        builder = builder.append(Element::builder("text", NS_ACTIVITY).append(text).build());
    }
    builder.build()
}

/// Build an empty XEP-0108 activity element that clears the current publication.
#[must_use]
pub fn build_activity_clear_element() -> Element {
    Element::builder("activity", NS_ACTIVITY).build()
}

/// Build an XEP-0118 tune element containing the supplied optional track metadata.
#[must_use]
pub fn build_tune_element(tune: &UserTune) -> Element {
    let mut builder = Element::builder("tune", NS_TUNE);
    if let Some(artist) = tune.artist.as_deref() {
        builder = builder.append(Element::builder("artist", NS_TUNE).append(artist).build());
    }
    if let Some(title) = tune.title.as_deref() {
        builder = builder.append(Element::builder("title", NS_TUNE).append(title).build());
    }
    if let Some(source) = tune.source.as_deref() {
        builder = builder.append(Element::builder("source", NS_TUNE).append(source).build());
    }
    if let Some(length) = tune.length {
        builder = builder.append(
            Element::builder("length", NS_TUNE)
                .append(length.to_string())
                .build(),
        );
    }
    if let Some(rating) = tune.rating {
        builder = builder.append(
            Element::builder("rating", NS_TUNE)
                .append(rating.to_string())
                .build(),
        );
    }
    if let Some(track) = tune.track.as_deref() {
        builder = builder.append(Element::builder("track", NS_TUNE).append(track).build());
    }
    if let Some(uri) = tune.uri.as_deref() {
        builder = builder.append(Element::builder("uri", NS_TUNE).append(uri).build());
    }
    builder.build()
}

/// Build an empty XEP-0118 tune element that clears the current publication.
#[must_use]
pub fn build_tune_clear_element() -> Element {
    Element::builder("tune", NS_TUNE).build()
}

/// Build an IQ-set publishing the supplied payload to the selected PEP node.
#[must_use]
pub fn build_pep_publish_iq(id: &str, node: &str, payload: Element) -> Element {
    build_pep_publish_iq_with_item_id(id, node, "current", payload)
}

/// PEP publish envelope with an explicit pubsub item id, for nodes whose
/// item id is content-addressed rather than the singleton `current` —
/// XEP-0084 §3.1 mandates the SHA-1 of the image bytes as the item id.
#[must_use]
pub fn build_pep_publish_iq_with_item_id(
    id: &str,
    node: &str,
    item_id: &str,
    payload: Element,
) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("publish", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                                .append(payload)
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Build an IQ-set clearing the selected PEP user-state node using its empty payload.
#[must_use]
pub fn build_pep_clear_iq(id: &str, node: &str) -> Element {
    let payload = match node {
        NS_MOOD => build_mood_clear_element(),
        NS_ACTIVITY => build_activity_clear_element(),
        NS_TUNE => build_tune_clear_element(),
        _ => Element::builder("item", NS_PUBSUB).build(),
    };
    build_pep_publish_iq(id, node, payload)
}
// Every builder below takes the IQ `id` from the caller: ids identify a
// single request on the stream (RFC 6120 §8.2.3), so each request MUST
// mint a fresh one (the FFI and wasm callers use UUIDs) — a fixed id
// would misroute a slow reply onto a later request's waiter.
/// Build a PEP publication IQ-set carrying the supplied XEP-0107 mood.
#[must_use]
pub fn build_publish_mood_iq(id: &str, mood: &str, text: Option<&str>) -> Element {
    build_pep_publish_iq(id, NS_MOOD, build_mood_element(mood, text))
}

/// Build a PEP publication IQ-set clearing the XEP-0107 mood.
#[must_use]
pub fn build_retract_mood_iq(id: &str) -> Element {
    build_pep_clear_iq(id, NS_MOOD)
}

/// Build a PEP publication IQ-set carrying the supplied XEP-0108 activity.
#[must_use]
pub fn build_publish_activity_iq(
    id: &str,
    general: &str,
    specific: Option<&str>,
    text: Option<&str>,
) -> Element {
    build_pep_publish_iq(
        id,
        NS_ACTIVITY,
        build_activity_element_with_specific(general, specific, text),
    )
}

/// Build a PEP publication IQ-set clearing the XEP-0108 activity.
#[must_use]
pub fn build_retract_activity_iq(id: &str) -> Element {
    build_pep_clear_iq(id, NS_ACTIVITY)
}

/// Build a PEP publication IQ-set carrying the supplied XEP-0118 tune.
#[must_use]
pub fn build_publish_tune_iq(id: &str, tune: &UserTune) -> Element {
    build_pep_publish_iq(id, NS_TUNE, build_tune_element(tune))
}

/// Build a PEP publication IQ-set clearing the XEP-0118 tune.
#[must_use]
pub fn build_retract_tune_iq(id: &str) -> Element {
    build_pep_clear_iq(id, NS_TUNE)
}

/// Build an IQ-get retrieving the selected PEP user-state node.
#[must_use]
pub fn build_pep_items_iq(id: &str, target_jid: &str, node: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), target_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                        .build(),
                )
                .build(),
        )
        .build()
}

fn find_pep_iq_payload<'a>(
    iq: &'a Element,
    node: &str,
    name: &str,
    ns: &str,
) -> Option<&'a Element> {
    iq.get_child("pubsub", NS_PUBSUB)?
        .get_child("items", NS_PUBSUB)
        .filter(|items| items.attr("node") == Some(node))?
        .get_child("item", NS_PUBSUB)?
        .get_child(name, ns)
}

/// Read the XEP-0107 mood payload from a PEP-items IQ result; missing or cleared values return None.
#[must_use]
pub fn parse_pep_mood(iq: &Element) -> Option<UserMood> {
    parse_mood_payload(find_pep_iq_payload(iq, NS_MOOD, "mood", NS_MOOD)?)
}

/// Read the XEP-0108 activity payload from a PEP-items IQ result; missing or cleared values return None.
#[must_use]
pub fn parse_pep_activity(iq: &Element) -> Option<UserActivity> {
    parse_activity_payload(find_pep_iq_payload(
        iq,
        NS_ACTIVITY,
        "activity",
        NS_ACTIVITY,
    )?)
}

/// Read the XEP-0118 tune payload from a PEP-items IQ result; missing or cleared values return None.
#[must_use]
pub fn parse_pep_tune(iq: &Element) -> Option<UserTune> {
    parse_tune_payload(find_pep_iq_payload(iq, NS_TUNE, "tune", NS_TUNE)?)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
/// Native client operations for publishing and clearing PEP user state.
pub trait PepExt {
    /// Queue a PEP mood publication with optional explanatory text.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Disconnected` if the driver command channel is closed. Successful completion means the publication was queued, without waiting for the server IQ result.
    fn publish_mood<'a>(
        &'a self,
        mood: &'a str,
        text: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    /// Queue an empty PEP mood publication to clear the current mood.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Disconnected` if the driver command channel is closed. Successful completion means the publication was queued, without waiting for the server IQ result.
    fn clear_mood(&self) -> impl std::future::Future<Output = ClientResult<()>> + Send + '_;
    /// Queue a PEP activity publication with optional explanatory text.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Disconnected` if the driver command channel is closed. Successful completion means the publication was queued, without waiting for the server IQ result.
    fn publish_activity<'a>(
        &'a self,
        activity: &'a str,
        text: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    /// Queue an empty PEP activity publication to clear the current activity.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Disconnected` if the driver command channel is closed. Successful completion means the publication was queued, without waiting for the server IQ result.
    fn clear_activity(&self) -> impl std::future::Future<Output = ClientResult<()>> + Send + '_;
    /// Queue a PEP tune publication with the supplied track metadata.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Disconnected` if the driver command channel is closed. Successful completion means the publication was queued, without waiting for the server IQ result.
    fn publish_tune<'a>(
        &'a self,
        artist: Option<&'a str>,
        title: Option<&'a str>,
        source: Option<&'a str>,
        length: Option<u32>,
        uri: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    /// Queue an empty PEP tune publication to clear the current tune.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Disconnected` if the driver command channel is closed. Successful completion means the publication was queued, without waiting for the server IQ result.
    fn clear_tune(&self) -> impl std::future::Future<Output = ClientResult<()>> + Send + '_;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl PepExt for ClientHandle {
    async fn publish_mood(&self, mood: &str, text: Option<&str>) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        self.send_stanza(build_pep_publish_iq(
            &id,
            NS_MOOD,
            build_mood_element(mood, text),
        ))
        .await
    }

    async fn clear_mood(&self) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        self.send_stanza(build_pep_clear_iq(&id, NS_MOOD)).await
    }

    async fn publish_activity(&self, activity: &str, text: Option<&str>) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        self.send_stanza(build_pep_publish_iq(
            &id,
            NS_ACTIVITY,
            build_activity_element(activity, text),
        ))
        .await
    }

    async fn clear_activity(&self) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        self.send_stanza(build_pep_clear_iq(&id, NS_ACTIVITY)).await
    }

    async fn publish_tune(
        &self,
        artist: Option<&str>,
        title: Option<&str>,
        source: Option<&str>,
        length: Option<u32>,
        uri: Option<&str>,
    ) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        self.send_stanza(build_pep_publish_iq(
            &id,
            NS_TUNE,
            build_tune_element(&UserTune {
                artist: artist.map(str::to_string),
                title: title.map(str::to_string),
                source: source.map(str::to_string),
                length,
                rating: None,
                track: None,
                uri: uri.map(str::to_string),
            }),
        ))
        .await
    }

    async fn clear_tune(&self) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        self.send_stanza(build_pep_clear_iq(&id, NS_TUNE)).await
    }
}

#[cfg(test)]
mod tests;
