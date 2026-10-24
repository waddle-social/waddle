//! XEP-0357 client FFI: VAPID lookup classification and the XEP-0198
//! resumability a push-driven client must opt out of.

use jid::BareJid;
use minidom::rxml::xml_ncname;
use minidom::Element;
use url::Url;
use waddle_xmpp_client::discovery::{
    CLIENT_NS, DATA_FORMS_NS, DISCO_INFO_NS, PUSH_VAPID_FIELD_KID, PUSH_VAPID_FIELD_PUBLIC_KEY,
    PUSH_VAPID_FORM_TYPE,
};
use waddle_xmpp_client::{
    AccessToken, ClientConfig, ClientError, ClientResource, ConnectionConfig, OAuthBearerConfig,
    WebSocketConfig,
};

use crate::push::vapid_lookup_of;
use crate::{apply_resumability, WaddlePushVapidLookup};

const SERVICE: &str = "push.waddle.test";
const KID: &str = "0b3c1d7e-8f4a-4d2b-9c6e-1a2b3c4d5e6f";
/// base64url of a 65-byte uncompressed SEC1 point (0x04 then zeros).
const PUBLIC_KEY: &str =
    "BAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn service() -> BareJid {
    BareJid::new(SERVICE).expect("valid service jid")
}

fn field(var: &str, value: &str, hidden: bool) -> Element {
    let mut field =
        Element::builder("field", DATA_FORMS_NS).attr(xml_ncname!("var").to_owned(), var);
    if hidden {
        field = field.attr(xml_ncname!("type").to_owned(), "hidden");
    }
    field
        .append(
            Element::builder("value", DATA_FORMS_NS)
                .append(value)
                .build(),
        )
        .build()
}

fn disco_reply(from: &str, advertised: bool) -> Element {
    let mut query = Element::builder("query", DISCO_INFO_NS);
    if advertised {
        query = query.append(
            Element::builder("x", DATA_FORMS_NS)
                .attr(xml_ncname!("type").to_owned(), "result")
                .append(field("FORM_TYPE", PUSH_VAPID_FORM_TYPE, true))
                .append(field(PUSH_VAPID_FIELD_PUBLIC_KEY, PUBLIC_KEY, false))
                .append(field(PUSH_VAPID_FIELD_KID, KID, false))
                .build(),
        );
    }
    Element::builder("iq", CLIENT_NS)
        .attr(xml_ncname!("type").to_owned(), "result")
        .attr(xml_ncname!("id").to_owned(), "disco-1")
        .attr(xml_ncname!("from").to_owned(), from)
        .append(query.build())
        .build()
}

#[test]
fn xep0357_vapid_lookup_surfaces_the_advertised_key() {
    let (lookup, diagnostic) = vapid_lookup_of(Ok(disco_reply(SERVICE, true)), &service());
    assert_eq!(
        lookup,
        WaddlePushVapidLookup::Advertised {
            public_key: PUBLIC_KEY.to_owned(),
            kid: KID.to_owned(),
        },
    );
    assert_eq!(diagnostic, None);
}

#[test]
fn xep0357_vapid_lookup_distinguishes_no_provider_from_failure() {
    let (lookup, diagnostic) = vapid_lookup_of(Ok(disco_reply(SERVICE, false)), &service());
    assert_eq!(lookup, WaddlePushVapidLookup::NotAdvertised);
    assert_eq!(diagnostic, None, "no provider is not an error");

    let (lookup, diagnostic) = vapid_lookup_of(Err(ClientError::Disconnected), &service());
    assert_eq!(lookup, WaddlePushVapidLookup::Failed);
    assert!(diagnostic.is_some());
}

#[test]
fn xep0357_vapid_lookup_refuses_a_key_from_another_entity() {
    let (lookup, diagnostic) =
        vapid_lookup_of(Ok(disco_reply("evil.waddle.test", true)), &service());
    assert_eq!(lookup, WaddlePushVapidLookup::Failed);
    assert!(diagnostic.is_some());
}

fn client_config() -> ClientConfig {
    let auth = OAuthBearerConfig::try_new(
        BareJid::new("alice@waddle.test").expect("valid jid"),
        ClientResource::try_new("test").expect("valid resource"),
        AccessToken::new("token"),
    )
    .expect("valid auth");
    let transport =
        WebSocketConfig::try_new(Url::parse("wss://xmpp.waddle.test").expect("valid url"))
            .expect("valid transport");
    ClientConfig::try_new(
        ConnectionConfig::new(BareJid::new("waddle.test").expect("valid domain")),
        transport,
        auth,
    )
    .expect("valid config")
}

#[test]
fn xep0198_non_resumable_sessions_request_no_resumption() {
    let mut config = client_config();
    apply_resumability(&mut config, false);
    let sm = &config.session.stream_management;
    assert!(sm.enable_stream_management, "acks still matter");
    assert!(!sm.allow_resume);
    assert_eq!(sm.resume_max, None);
    assert_eq!(sm.resume_state, None);
}

#[test]
fn xep0198_resumable_sessions_keep_the_default_window() {
    let mut config = client_config();
    let default_max = config.session.stream_management.resume_max;
    apply_resumability(&mut config, true);
    assert!(config.session.stream_management.allow_resume);
    assert_eq!(config.session.stream_management.resume_max, default_max);
}
