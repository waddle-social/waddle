//! Per-XEP FFI test suites for the profile publishing exports:
//! user avatar (XEP-0084), vCard4 (XEP-0292), mood (XEP-0107),
//! activity (XEP-0108), and tune (XEP-0118).
//!
//! Pins the pure FFI mapping layer (record ↔ typed-core projections,
//! rating clamp, element-name gate), the exact wire shapes the
//! exported methods emit (through the shared core builders and the
//! recording avatar send seam), and the typed failure behavior
//! (invalid JID / argument, not connected) through an offline client,
//! mirroring `room_admin_tests.rs`.

use std::cell::RefCell;
use std::sync::Arc;

use minidom::Element;

use waddle_xmpp_client::avatar::{
    compute_avatar_item_id, AVATAR_REMOVE_ITEM_ID, NS_AVATAR_DATA, NS_AVATAR_METADATA, NS_PUBSUB,
};
use waddle_xmpp_client::pep::{
    build_publish_activity_iq, build_publish_mood_iq, build_publish_tune_iq,
    build_retract_activity_iq, build_retract_mood_iq, build_retract_tune_iq, UserActivity,
    UserMood, NS_ACTIVITY, NS_MOOD, NS_TUNE,
};
use waddle_xmpp_client::xep::xep0292::{parse_vcard4_element, NS_VCARD4};

use crate::profile_verbs::{
    activity_from_core, mood_from_core, publish_avatar_iqs, require_pep_element_name,
    tune_from_core, tune_is_empty, tune_to_core, vcard4_from_core, vcard4_to_core, WaddleTune,
    WaddleVCard4,
};
use crate::{WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener};

struct NullListener;

impl WaddleEventListener for NullListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

fn offline_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(NullListener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
    })
}

/// Walk a PEP publish IQ down to its `<item>`, asserting the envelope
/// targets `node`.
fn publish_item<'a>(iq: &'a Element, node: &str) -> &'a Element {
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("set"));
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish");
    assert_eq!(publish.attr("node"), Some(node));
    publish.get_child("item", NS_PUBSUB).expect("item")
}

fn iq_result() -> Element {
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "r")
        .build()
}

fn full_tune() -> WaddleTune {
    WaddleTune {
        artist: Some("The Beatles".to_string()),
        title: Some("Come Together".to_string()),
        source: Some("Abbey Road".to_string()),
        track: Some("1".to_string()),
        uri: Some("https://music.example.com/abbey-road/1".to_string()),
        length_seconds: Some(259),
        rating: Some(9),
    }
}

// ── XEP-0084 user avatar ─────────────────────────────────────────────

mod xep0084 {
    use super::*;

    /// Drive [`publish_avatar_iqs`] with a recording seam; every send
    /// is acked with an empty `<iq type='result'/>`.
    async fn record_publish(
        data: &[u8],
        mime: &str,
        width: u32,
        height: u32,
    ) -> (Result<(), WaddleError>, Vec<Element>) {
        let sent = RefCell::new(Vec::new());
        let result = publish_avatar_iqs(data, mime, width, height, |iq| {
            sent.borrow_mut().push(iq);
            async { Ok(iq_result()) }
        })
        .await;
        (result, sent.into_inner())
    }

    #[tokio::test]
    async fn publish_sends_data_then_metadata_at_sha1_item_id() {
        let data = b"hello".as_slice();
        let (result, sent) = record_publish(data, "image/png", 64, 48).await;
        result.expect("publish succeeds");
        assert_eq!(sent.len(), 2);

        // First on the wire MUST be the data item (XEP-0084 §3.2).
        let item_id = compute_avatar_item_id(data);
        let data_item = publish_item(&sent[0], NS_AVATAR_DATA);
        assert_eq!(data_item.attr("id"), Some(item_id.as_str()));
        assert_eq!(
            data_item
                .get_child("data", NS_AVATAR_DATA)
                .expect("data payload")
                .text(),
            "aGVsbG8="
        );

        // Then the metadata item with the REQUIRED info attributes.
        let meta_item = publish_item(&sent[1], NS_AVATAR_METADATA);
        assert_eq!(meta_item.attr("id"), Some(item_id.as_str()));
        let info = meta_item
            .get_child("metadata", NS_AVATAR_METADATA)
            .and_then(|metadata| metadata.get_child("info", NS_AVATAR_METADATA))
            .expect("info");
        assert_eq!(info.attr("bytes"), Some("5"));
        assert_eq!(info.attr("id"), Some(item_id.as_str()));
        assert_eq!(info.attr("type"), Some("image/png"));
        assert_eq!(info.attr("width"), Some("64"));
        assert_eq!(info.attr("height"), Some("48"));
    }

    #[tokio::test]
    async fn publish_omits_zero_dimensions() {
        let (result, sent) = record_publish(b"hello", "image/png", 0, 0).await;
        result.expect("publish succeeds");
        let info = publish_item(&sent[1], NS_AVATAR_METADATA)
            .get_child("metadata", NS_AVATAR_METADATA)
            .and_then(|metadata| metadata.get_child("info", NS_AVATAR_METADATA))
            .expect("info");
        assert_eq!(info.attr("width"), None);
        assert_eq!(info.attr("height"), None);
    }

    #[tokio::test]
    async fn publish_stops_before_metadata_when_data_publish_fails() {
        let sent = RefCell::new(Vec::new());
        let result = publish_avatar_iqs(b"hello", "image/png", 0, 0, |iq| {
            sent.borrow_mut().push(iq);
            async {
                Err(WaddleError::Stanza {
                    condition: "forbidden".to_string(),
                    text: None,
                })
            }
        })
        .await;
        assert!(matches!(result, Err(WaddleError::Stanza { .. })));
        // The metadata item was never sent: a subscriber can never see
        // metadata describing bytes that are not retrievable.
        assert_eq!(sent.into_inner().len(), 1);
    }

    #[tokio::test]
    async fn publish_rejects_empty_data_and_mime_before_sending() {
        let (result, sent) = record_publish(b"", "image/png", 0, 0).await;
        assert_eq!(result.unwrap_err(), WaddleError::InvalidArgument);
        assert!(sent.is_empty());

        let (result, sent) = record_publish(b"hello", "  ", 0, 0).await;
        assert_eq!(result.unwrap_err(), WaddleError::InvalidArgument);
        assert!(sent.is_empty());
    }

    #[test]
    fn disable_avatar_publishes_empty_metadata_at_current() {
        let iq = waddle_xmpp_client::avatar::build_disable_avatar_iq();
        let item = publish_item(&iq, NS_AVATAR_METADATA);
        assert_eq!(item.attr("id"), Some(AVATAR_REMOVE_ITEM_ID));
        let metadata = item
            .get_child("metadata", NS_AVATAR_METADATA)
            .expect("metadata");
        assert_eq!(metadata.children().count(), 0);
    }

    #[tokio::test]
    async fn publish_avatar_requires_connection() {
        let client = offline_client();
        assert_eq!(
            client
                .publish_avatar(b"hello".to_vec(), "image/png".to_string(), 0, 0)
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
    }

    #[tokio::test]
    async fn disable_avatar_requires_connection() {
        let client = offline_client();
        assert_eq!(
            client.disable_avatar().await.unwrap_err(),
            WaddleError::NotConnected
        );
    }
}

// ── XEP-0292 vCard4 ──────────────────────────────────────────────────

mod xep0292 {
    use super::*;

    fn full_vcard() -> WaddleVCard4 {
        WaddleVCard4 {
            full_name: Some("Juliet Capulet".to_string()),
            nickname: Some("Juliet".to_string()),
            pronouns: Some("she/her".to_string()),
            note: Some("Star-crossed".to_string()),
            url: Some("https://juliet.example.com".to_string()),
            photo_uri: Some("cid:juliet-avatar@example.com".to_string()),
        }
    }

    #[test]
    fn vcard_round_trips_through_core_projection() {
        let core = vcard4_to_core(full_vcard());
        assert_eq!(core.full_name.as_deref(), Some("Juliet Capulet"));
        assert_eq!(core.pronouns.as_deref(), Some("she/her"));
        let back = vcard4_from_core(core);
        assert_eq!(back.nickname.as_deref(), Some("Juliet"));
        assert_eq!(back.note.as_deref(), Some("Star-crossed"));
        assert_eq!(back.url.as_deref(), Some("https://juliet.example.com"));
        assert_eq!(
            back.photo_uri.as_deref(),
            Some("cid:juliet-avatar@example.com")
        );
    }

    #[test]
    fn publish_wire_shape_carries_every_property_at_current() {
        let iq = waddle_xmpp_client::xep::xep0292::build_publish_vcard4_iq(
            &vcard4_to_core(full_vcard()),
            "req-1",
        );
        let item = publish_item(&iq, "urn:xmpp:vcard4");
        assert_eq!(item.attr("id"), Some("current"));
        let vcard = item.get_child("vcard", NS_VCARD4).expect("vcard payload");
        let parsed = parse_vcard4_element(vcard);
        assert_eq!(parsed.full_name.as_deref(), Some("Juliet Capulet"));
        assert_eq!(parsed.pronouns.as_deref(), Some("she/her"));
        assert_eq!(
            parsed.photo_uri.as_deref(),
            Some("cid:juliet-avatar@example.com")
        );
    }

    #[tokio::test]
    async fn fetch_vcard4_rejects_invalid_jid() {
        let client = offline_client();
        assert_eq!(
            client
                .fetch_vcard4("not a jid".to_string())
                .await
                .unwrap_err(),
            WaddleError::InvalidJid
        );
    }

    #[tokio::test]
    async fn fetch_and_publish_require_connection() {
        let client = offline_client();
        assert_eq!(
            client
                .fetch_vcard4("juliet@waddle.test".to_string())
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
        assert_eq!(
            client.publish_vcard4(full_vcard()).await.unwrap_err(),
            WaddleError::NotConnected
        );
    }
}

// ── XEP-0107 user mood ───────────────────────────────────────────────

mod xep0107 {
    use super::*;

    #[test]
    fn publish_wire_shape_nests_kind_and_text() {
        let iq = build_publish_mood_iq("happy", Some("feeling good"));
        let mood = publish_item(&iq, NS_MOOD)
            .get_child("mood", NS_MOOD)
            .expect("mood payload");
        assert!(mood.get_child("happy", NS_MOOD).is_some());
        assert_eq!(
            mood.get_child("text", NS_MOOD).map(Element::text),
            Some("feeling good".to_string())
        );
    }

    #[test]
    fn retract_wire_shape_publishes_empty_mood() {
        let iq = build_retract_mood_iq();
        let mood = publish_item(&iq, NS_MOOD)
            .get_child("mood", NS_MOOD)
            .expect("mood payload");
        assert_eq!(mood.children().count(), 0);
    }

    #[test]
    fn mood_projection_maps_core_fields() {
        let mood = mood_from_core(UserMood {
            mood: "happy".to_string(),
            text: Some("!".to_string()),
        });
        assert_eq!(mood.kind, "happy");
        assert_eq!(mood.text.as_deref(), Some("!"));
    }

    #[tokio::test]
    async fn publish_mood_rejects_invalid_kind_before_connecting() {
        let client = offline_client();
        for kind in ["", "  ", "Happy", "not a mood", "<mood/>"] {
            assert_eq!(
                client
                    .publish_mood(kind.to_string(), None)
                    .await
                    .unwrap_err(),
                WaddleError::InvalidArgument,
                "kind {kind:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn publish_and_retract_require_connection() {
        let client = offline_client();
        assert_eq!(
            client
                .publish_mood("happy".to_string(), Some("!".to_string()))
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
        assert_eq!(
            client.retract_mood().await.unwrap_err(),
            WaddleError::NotConnected
        );
    }
}

// ── XEP-0108 user activity ───────────────────────────────────────────

mod xep0108 {
    use super::*;

    #[test]
    fn publish_wire_shape_nests_specific_inside_general() {
        let iq = build_publish_activity_iq("working", Some("coding"), Some("rewriting waddle"));
        let activity = publish_item(&iq, NS_ACTIVITY)
            .get_child("activity", NS_ACTIVITY)
            .expect("activity payload");
        let general = activity
            .get_child("working", NS_ACTIVITY)
            .expect("general category");
        assert!(general.get_child("coding", NS_ACTIVITY).is_some());
        assert_eq!(
            activity.get_child("text", NS_ACTIVITY).map(Element::text),
            Some("rewriting waddle".to_string())
        );
    }

    #[test]
    fn retract_wire_shape_publishes_empty_activity() {
        let iq = build_retract_activity_iq();
        let activity = publish_item(&iq, NS_ACTIVITY)
            .get_child("activity", NS_ACTIVITY)
            .expect("activity payload");
        assert_eq!(activity.children().count(), 0);
    }

    #[test]
    fn activity_projection_maps_core_fields() {
        let activity = activity_from_core(UserActivity {
            activity: "working".to_string(),
            specific: Some("coding".to_string()),
            text: None,
        });
        assert_eq!(activity.general, "working");
        assert_eq!(activity.specific.as_deref(), Some("coding"));
        assert_eq!(activity.text, None);
    }

    #[tokio::test]
    async fn publish_activity_rejects_invalid_names_before_connecting() {
        let client = offline_client();
        assert_eq!(
            client
                .publish_activity(String::new(), None, None)
                .await
                .unwrap_err(),
            WaddleError::InvalidArgument
        );
        assert_eq!(
            client
                .publish_activity("working".to_string(), Some("no way".to_string()), None)
                .await
                .unwrap_err(),
            WaddleError::InvalidArgument
        );
    }

    #[tokio::test]
    async fn publish_and_retract_require_connection() {
        let client = offline_client();
        assert_eq!(
            client
                .publish_activity("working".to_string(), Some("coding".to_string()), None)
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
        assert_eq!(
            client.retract_activity().await.unwrap_err(),
            WaddleError::NotConnected
        );
    }
}

// ── XEP-0118 user tune ───────────────────────────────────────────────

mod xep0118 {
    use super::*;

    #[test]
    fn publish_wire_shape_carries_every_field() {
        let tune = tune_to_core(full_tune());
        let iq = build_publish_tune_iq(
            tune.artist.as_deref(),
            tune.title.as_deref(),
            tune.source.as_deref(),
            tune.length,
            tune.rating,
            tune.track.as_deref(),
            tune.uri.as_deref(),
        );
        let payload = publish_item(&iq, NS_TUNE)
            .get_child("tune", NS_TUNE)
            .expect("tune payload");
        let text_of = |name: &str| payload.get_child(name, NS_TUNE).map(Element::text);
        assert_eq!(text_of("artist"), Some("The Beatles".to_string()));
        assert_eq!(text_of("title"), Some("Come Together".to_string()));
        assert_eq!(text_of("source"), Some("Abbey Road".to_string()));
        assert_eq!(text_of("track"), Some("1".to_string()));
        assert_eq!(
            text_of("uri"),
            Some("https://music.example.com/abbey-road/1".to_string())
        );
        assert_eq!(text_of("length"), Some("259".to_string()));
        assert_eq!(text_of("rating"), Some("9".to_string()));
    }

    #[test]
    fn retract_wire_shape_publishes_empty_tune() {
        let iq = build_retract_tune_iq();
        let tune = publish_item(&iq, NS_TUNE)
            .get_child("tune", NS_TUNE)
            .expect("tune payload");
        assert_eq!(tune.children().count(), 0);
    }

    #[test]
    fn rating_clamps_into_xep0118_interval() {
        let rate = |rating| {
            tune_to_core(WaddleTune {
                rating: Some(rating),
                ..full_tune()
            })
            .rating
        };
        assert_eq!(rate(0), Some(1));
        assert_eq!(rate(1), Some(1));
        assert_eq!(rate(10), Some(10));
        assert_eq!(rate(200), Some(10));
        assert_eq!(
            tune_to_core(WaddleTune {
                rating: None,
                ..full_tune()
            })
            .rating,
            None
        );
    }

    #[test]
    fn tune_projection_round_trips_core_fields() {
        let back = tune_from_core(tune_to_core(full_tune()));
        assert_eq!(back.artist.as_deref(), Some("The Beatles"));
        assert_eq!(back.length_seconds, Some(259));
        assert_eq!(back.rating, Some(9));
    }

    #[test]
    fn empty_tune_is_detected() {
        let empty = WaddleTune {
            artist: None,
            title: None,
            source: None,
            track: None,
            uri: None,
            length_seconds: None,
            rating: None,
        };
        assert!(tune_is_empty(&empty));
        assert!(!tune_is_empty(&full_tune()));
    }

    #[tokio::test]
    async fn publish_tune_rejects_empty_payload_before_connecting() {
        let client = offline_client();
        let empty = WaddleTune {
            artist: None,
            title: None,
            source: None,
            track: None,
            uri: None,
            length_seconds: None,
            rating: None,
        };
        assert_eq!(
            client.publish_tune(empty).await.unwrap_err(),
            WaddleError::InvalidArgument
        );
    }

    #[tokio::test]
    async fn publish_and_retract_require_connection() {
        let client = offline_client();
        assert_eq!(
            client.publish_tune(full_tune()).await.unwrap_err(),
            WaddleError::NotConnected
        );
        assert_eq!(
            client.retract_tune().await.unwrap_err(),
            WaddleError::NotConnected
        );
    }
}

// ── Element-name gate + PEP profile snapshot ─────────────────────────

mod validation {
    use super::*;

    #[test]
    fn element_name_gate_accepts_registry_style_names() {
        for name in ["happy", "in_love", "on-a-trip", "working", "day_off", "x2"] {
            require_pep_element_name(name).expect("registry-style name accepted");
        }
    }

    #[test]
    fn element_name_gate_rejects_wire_breaking_names() {
        for name in ["", " ", "Happy", "a b", "mood/>", "ünïcode", "<x"] {
            assert_eq!(
                require_pep_element_name(name).unwrap_err(),
                WaddleError::InvalidArgument,
                "name {name:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn fetch_user_pep_profile_rejects_invalid_jid() {
        let client = offline_client();
        assert_eq!(
            client
                .fetch_user_pep_profile("not a jid".to_string())
                .await
                .unwrap_err(),
            WaddleError::InvalidJid
        );
    }

    #[tokio::test]
    async fn fetch_user_pep_profile_requires_connection() {
        let client = offline_client();
        assert_eq!(
            client
                .fetch_user_pep_profile("juliet@waddle.test".to_string())
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
    }
}
