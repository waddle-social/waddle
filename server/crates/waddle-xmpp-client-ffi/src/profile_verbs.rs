//! Profile publishing verb exports: user avatar (XEP-0084), vCard4
//! (XEP-0292), mood (XEP-0107), activity (XEP-0108), and tune
//! (XEP-0118) over their PEP nodes (XEP-0163).
//!
//! Every method validates its untyped FFI inputs (JID strings, mood /
//! activity element names) into typed values immediately, then
//! delegates to the shared `waddle-xmpp-client` builders — the same
//! code paths the wasm chat client drives. All methods are
//! `Result<_, WaddleError>` surfaces so profile editors can explain
//! *why* a publish was refused (`forbidden`, `not-acceptable`, …).
//!
//! Avatar publication follows the XEP-0084 §3.2 MUST-order: the data
//! item is published and acknowledged first, and only then the
//! metadata item — subscribers that react to the metadata notification
//! can always fetch the bytes it describes.

use std::future::Future;

use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::avatar::{
    build_disable_avatar_iq, build_publish_avatar_data_iq, build_publish_avatar_metadata_iq,
    compute_avatar_item_id, AvatarPublishInfo,
};
use waddle_xmpp_client::pep::{
    build_pep_items_iq, build_publish_activity_iq, build_publish_mood_iq, build_publish_tune_iq,
    build_retract_activity_iq, build_retract_mood_iq, build_retract_tune_iq, parse_pep_activity,
    parse_pep_mood, parse_pep_tune, UserActivity, UserMood, UserTune, NS_ACTIVITY, NS_MOOD,
    NS_TUNE,
};
use waddle_xmpp_client::xep::xep0292::{
    build_fetch_vcard4_iq, build_publish_vcard4_iq, parse_pep_vcard4, VCard4,
};
use waddle_xmpp_client::ClientError;

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};

// ── Typed FFI payloads ───────────────────────────────────────────────

/// XEP-0292 vCard4 profile fields (the subset the profile editor
/// surfaces). `None` means the property is absent on the wire.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleVCard4 {
    pub full_name: Option<String>,
    pub nickname: Option<String>,
    pub pronouns: Option<String>,
    pub note: Option<String>,
    pub url: Option<String>,
    pub photo_uri: Option<String>,
}

/// XEP-0107 user mood: `kind` is the defined mood element name
/// (e.g. `happy`), `text` the optional free-form annotation.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleMood {
    pub kind: String,
    pub text: Option<String>,
}

/// XEP-0108 user activity: `general` / `specific` are the defined
/// category element names (e.g. `working` / `coding`).
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleActivity {
    pub general: String,
    pub specific: Option<String>,
    pub text: Option<String>,
}

/// XEP-0118 user tune. `rating` is 1–10 per §3.1; out-of-range values
/// are clamped into that interval before publishing.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleTune {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub source: Option<String>,
    pub track: Option<String>,
    pub uri: Option<String>,
    pub length_seconds: Option<u32>,
    pub rating: Option<u8>,
}

/// Snapshot of a user's mood/activity/tune PEP nodes, fetched via
/// three XEP-0060 items requests (wasm `fetch_user_pep_profile`
/// parity). A node that is absent, empty, or answers with a stanza
/// error surfaces as `None`.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddlePepProfile {
    pub mood: Option<WaddleMood>,
    pub activity: Option<WaddleActivity>,
    pub tune: Option<WaddleTune>,
}

// ── FFI ↔ core mappings ──────────────────────────────────────────────
//
// Pure functions between the FFI record shapes and the shared typed
// values. The exported methods call these; the per-XEP test suite
// asserts their output against the XEP wire shapes, so the exact
// stanzas the FFI emits are pinned without a live connection.

pub(crate) fn vcard4_to_core(vcard: WaddleVCard4) -> VCard4 {
    VCard4 {
        full_name: vcard.full_name,
        nickname: vcard.nickname,
        pronouns: vcard.pronouns,
        note: vcard.note,
        url: vcard.url,
        photo_uri: vcard.photo_uri,
    }
}

pub(crate) fn vcard4_from_core(vcard: VCard4) -> WaddleVCard4 {
    WaddleVCard4 {
        full_name: vcard.full_name,
        nickname: vcard.nickname,
        pronouns: vcard.pronouns,
        note: vcard.note,
        url: vcard.url,
        photo_uri: vcard.photo_uri,
    }
}

pub(crate) fn mood_from_core(mood: UserMood) -> WaddleMood {
    WaddleMood {
        kind: mood.mood,
        text: mood.text,
    }
}

pub(crate) fn activity_from_core(activity: UserActivity) -> WaddleActivity {
    WaddleActivity {
        general: activity.activity,
        specific: activity.specific,
        text: activity.text,
    }
}

pub(crate) fn tune_from_core(tune: UserTune) -> WaddleTune {
    WaddleTune {
        artist: tune.artist,
        title: tune.title,
        source: tune.source,
        track: tune.track,
        uri: tune.uri,
        length_seconds: tune.length,
        rating: tune.rating,
    }
}

/// Project the FFI tune into the core value, clamping `rating` into
/// the XEP-0118 §3.1 1–10 interval so an out-of-range caller value can
/// never reach the wire.
pub(crate) fn tune_to_core(tune: WaddleTune) -> UserTune {
    UserTune {
        artist: tune.artist,
        title: tune.title,
        source: tune.source,
        length: tune.length_seconds,
        rating: tune.rating.map(|rating| rating.clamp(1, 10)),
        track: tune.track,
        uri: tune.uri,
    }
}

/// Returns `true` when every tune field is `None`. Publishing such a
/// tune would emit the empty `<tune/>` — the XEP-0118 §3.2 *stop*
/// shape — so [`WaddleClient::publish_tune`] rejects it and callers
/// use [`WaddleClient::retract_tune`] for that intent instead.
pub(crate) fn tune_is_empty(tune: &WaddleTune) -> bool {
    tune.artist.is_none()
        && tune.title.is_none()
        && tune.source.is_none()
        && tune.track.is_none()
        && tune.uri.is_none()
        && tune.length_seconds.is_none()
        && tune.rating.is_none()
}

/// Gate for caller-supplied strings that become XML element names on
/// the wire (XEP-0107 mood kinds, XEP-0108 activity categories). The
/// registries define only lowercase ASCII names with underscores;
/// anything outside `[a-z0-9_-]` is rejected before an element is
/// built, since `minidom` does not validate names at build time.
pub(crate) fn require_pep_element_name(value: &str) -> Result<(), WaddleError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(WaddleError::InvalidArgument);
    }
    Ok(())
}

// ── XEP-0084 publish sequence ────────────────────────────────────────

/// Publish an avatar through `send`, enforcing the XEP-0084 §3.2
/// MUST-order: the data item is sent and acknowledged first, then the
/// metadata item. Generic over the send seam so the test suite can
/// record the exact stanzas and their order without a live connection.
///
/// `width` / `height` of `0` mean "unknown" and omit the optional
/// `<info/>` dimension attributes.
pub(crate) async fn publish_avatar_iqs<F, Fut>(
    data: &[u8],
    mime_type: &str,
    width: u32,
    height: u32,
    mut send: F,
) -> Result<(), WaddleError>
where
    F: FnMut(Element) -> Fut,
    Fut: Future<Output = Result<Element, WaddleError>>,
{
    let bytes = u32::try_from(data.len()).map_err(|_| WaddleError::InvalidArgument)?;
    if data.is_empty() || mime_type.trim().is_empty() {
        return Err(WaddleError::InvalidArgument);
    }
    let item_id = compute_avatar_item_id(data);
    send(build_publish_avatar_data_iq(&item_id, data)).await?;
    let info = AvatarPublishInfo {
        bytes,
        id: item_id,
        mime_type: mime_type.to_string(),
        width: (width > 0).then_some(width),
        height: (height > 0).then_some(height),
    };
    send(build_publish_avatar_metadata_iq(&info)).await?;
    Ok(())
}

// ── Exported verbs ───────────────────────────────────────────────────

impl WaddleClient {
    /// Send one profile publish IQ and await its ack.
    async fn send_profile_iq(&self, iq: Element) -> Result<(), WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0084 §3: publish `data` as the account's avatar — the
    /// base64 data item first, then (after the server ack) the
    /// metadata item, both at the SHA-1-of-bytes item id. Pass `0`
    /// for `width` / `height` when the dimensions are unknown.
    pub async fn publish_avatar(
        &self,
        data: Vec<u8>,
        mime_type: String,
        width: u32,
        height: u32,
    ) -> Result<(), WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        publish_avatar_iqs(&data, &mime_type, width, height, |iq| {
            let handle = handle.clone();
            async move {
                send_iq_with_timeout(&handle, iq)
                    .await
                    .map_err(|e| client_error_to_waddle(&e))
            }
        })
        .await
    }

    /// XEP-0084 §4.3: publish the empty `<metadata/>` "no avatar"
    /// item so subscribers drop their cached avatar.
    pub async fn disable_avatar(&self) -> Result<(), WaddleError> {
        self.send_profile_iq(build_disable_avatar_iq()).await
    }

    /// XEP-0292: fetch `jid`'s vCard4 from its `urn:xmpp:vcard4` PEP
    /// node. `Ok(None)` when the node is absent, empty, or answers
    /// with a stanza error (wasm `fetch_vcard4` parity).
    pub async fn fetch_vcard4(&self, jid: String) -> Result<Option<WaddleVCard4>, WaddleError> {
        let jid = self.require_bare_jid(&jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let request_id = Uuid::new_v4().to_string();
        let iq = build_fetch_vcard4_iq(jid.as_str(), &request_id);
        match send_iq_with_timeout(&handle, iq).await {
            Ok(result) => Ok(parse_pep_vcard4(&result).map(vcard4_from_core)),
            Err(ClientError::StanzaError(_)) => Ok(None),
            Err(e) => Err(client_error_to_waddle(&e)),
        }
    }

    /// XEP-0292: publish the account's own vCard4 at item id
    /// `current`. Empty (`None`) properties are omitted on the wire.
    pub async fn publish_vcard4(&self, vcard: WaddleVCard4) -> Result<(), WaddleError> {
        let request_id = Uuid::new_v4().to_string();
        let iq = build_publish_vcard4_iq(&vcard4_to_core(vcard), &request_id);
        self.send_profile_iq(iq).await
    }

    /// XEP-0107: publish a user mood. `kind` must be a defined mood
    /// element name (e.g. `happy`).
    pub async fn publish_mood(
        &self,
        kind: String,
        text: Option<String>,
    ) -> Result<(), WaddleError> {
        require_pep_element_name(&kind)?;
        self.send_profile_iq(build_publish_mood_iq(&kind, text.as_deref()))
            .await
    }

    /// XEP-0107 §2.2: retract the mood by publishing the empty
    /// `<mood/>` payload.
    pub async fn retract_mood(&self) -> Result<(), WaddleError> {
        self.send_profile_iq(build_retract_mood_iq()).await
    }

    /// XEP-0108: publish a user activity. `general` (and `specific`
    /// when present) must be defined category element names.
    pub async fn publish_activity(
        &self,
        general: String,
        specific: Option<String>,
        text: Option<String>,
    ) -> Result<(), WaddleError> {
        require_pep_element_name(&general)?;
        if let Some(ref specific) = specific {
            require_pep_element_name(specific)?;
        }
        self.send_profile_iq(build_publish_activity_iq(
            &general,
            specific.as_deref(),
            text.as_deref(),
        ))
        .await
    }

    /// XEP-0108: retract the activity by publishing the empty
    /// `<activity/>` payload.
    pub async fn retract_activity(&self) -> Result<(), WaddleError> {
        self.send_profile_iq(build_retract_activity_iq()).await
    }

    /// XEP-0118: publish a user tune. At least one field must be set —
    /// an all-empty tune is the §3.2 *stop* shape, which is
    /// [`Self::retract_tune`]'s job. `rating` is clamped to 1–10.
    pub async fn publish_tune(&self, tune: WaddleTune) -> Result<(), WaddleError> {
        if tune_is_empty(&tune) {
            return Err(WaddleError::InvalidArgument);
        }
        let tune = tune_to_core(tune);
        self.send_profile_iq(build_publish_tune_iq(
            tune.artist.as_deref(),
            tune.title.as_deref(),
            tune.source.as_deref(),
            tune.length,
            tune.rating,
            tune.track.as_deref(),
            tune.uri.as_deref(),
        ))
        .await
    }

    /// XEP-0118 §3.2: stop publishing a tune via the empty `<tune/>`
    /// payload.
    pub async fn retract_tune(&self) -> Result<(), WaddleError> {
        self.send_profile_iq(build_retract_tune_iq()).await
    }

    /// Fetch `jid`'s mood/activity/tune PEP nodes in one call (wasm
    /// `fetch_user_pep_profile` parity): each node is queried with a
    /// XEP-0060 items request, and per-node failures (absent node,
    /// stanza error, empty payload) degrade to `None` rather than
    /// failing the whole snapshot.
    pub async fn fetch_user_pep_profile(
        &self,
        jid: String,
    ) -> Result<WaddlePepProfile, WaddleError> {
        let jid = self.require_bare_jid(&jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let mood = send_iq_with_timeout(&handle, build_pep_items_iq(jid.as_str(), NS_MOOD))
            .await
            .ok()
            .as_ref()
            .and_then(parse_pep_mood)
            .map(mood_from_core);
        let activity = send_iq_with_timeout(&handle, build_pep_items_iq(jid.as_str(), NS_ACTIVITY))
            .await
            .ok()
            .as_ref()
            .and_then(parse_pep_activity)
            .map(activity_from_core);
        let tune = send_iq_with_timeout(&handle, build_pep_items_iq(jid.as_str(), NS_TUNE))
            .await
            .ok()
            .as_ref()
            .and_then(parse_pep_tune)
            .map(tune_from_core);
        Ok(WaddlePepProfile {
            mood,
            activity,
            tune,
        })
    }
}
