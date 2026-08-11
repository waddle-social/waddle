use minidom::{rxml::xml_ncname, Element};
use sha2::{Digest, Sha256};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, DigestInputError, DigestVersion, NormalizedTarget, SemanticDigest,
};
use xmpp_parsers::message::{Id, Lang, Message, Thread};

const CLIENT: &str = "jabber:client";
const SID: &str = "urn:xmpp:sid:0";
const REPLY: &str = "urn:xmpp:reply:0";

fn context() -> DigestContext {
    DigestContext {
        target: NormalizedTarget::Absent,
        server_authorities: Vec::new(),
        stanza_lang: None,
    }
}

fn input(message: &Message, context: &DigestContext) -> DigestInput {
    DigestInput::from_parsed(message, context).expect("valid digest input")
}

fn digest(message: &Message, context: &DigestContext) -> SemanticDigest {
    waddle_xmpp::ingress::digest::v1::digest(&input(message, context))
}

macro_rules! assert_error {
    ($result:expr, $error:pat) => {
        assert!(matches!($result, Err($error)));
    };
}

fn hex(digest: &SemanticDigest) -> String {
    digest
        .to_storage()
        .1
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn body_message(body: &str) -> Message {
    Message::normal(None).with_body(Lang::new(), body.to_owned())
}

fn payload(name: &str, namespace: &str, text: &str) -> Element {
    Element::builder(name, namespace).append(text).build()
}

fn with_payloads(payloads: Vec<Element>) -> Message {
    Message::normal(None).with_payloads(payloads)
}

fn origin(id: &str) -> Element {
    Element::builder("origin-id", SID)
        .attr(xml_ncname!("id").to_owned(), id)
        .build()
}

fn stanza_id(id: &str, by: &str) -> Element {
    Element::builder("stanza-id", SID)
        .attr(xml_ncname!("id").to_owned(), id)
        .attr(xml_ncname!("by").to_owned(), by)
        .build()
}

fn reply(id: &str, to: Option<&str>) -> Element {
    let builder = Element::builder("reply", REPLY).attr(xml_ncname!("id").to_owned(), id);
    match to {
        Some(to) => builder.attr(xml_ncname!("to").to_owned(), to).build(),
        None => builder.build(),
    }
}

#[test]
fn neutral_wire_metadata_does_not_change_the_digest() {
    let baseline = body_message("hello\nworld");
    let baseline_digest = digest(&baseline, &context());

    let mut with_wire_id = baseline.clone();
    with_wire_id.id = Some(Id("wire-id".to_owned()));
    assert_eq!(digest(&with_wire_id, &context()), baseline_digest);

    for value in ["opaque-one", "opaque-two"] {
        let mut message = baseline.clone();
        message.payloads.push(origin(value));
        assert_eq!(digest(&message, &context()), baseline_digest);
    }

    let mut foreign_stanza_id = baseline.clone();
    foreign_stanza_id
        .payloads
        .push(stanza_id("foreign", "archive.example"));
    assert_eq!(digest(&foreign_stanza_id, &context()), baseline_digest);

    let mut delayed = baseline.clone();
    delayed
        .payloads
        .push(payload("delay", "urn:xmpp:delay", "later"));
    assert_eq!(digest(&delayed, &context()), baseline_digest);
}

#[test]
fn semantic_fields_are_sensitive() {
    let base = body_message("one");
    let base_digest = digest(&base, &context());
    assert_ne!(digest(&body_message("two"), &context()), base_digest);

    let mut body_language = base.clone();
    body_language
        .bodies
        .insert(Lang::from("nb"), "one".to_owned());
    assert_ne!(digest(&body_language, &context()), base_digest);

    let empty_body = body_message("");
    let absent_body = Message::normal(None);
    assert_ne!(
        digest(&empty_body, &context()),
        digest(&absent_body, &context())
    );

    let mut empty_subject = Message::normal(None);
    empty_subject.subjects.insert(Lang::new(), String::new());
    assert_ne!(
        digest(&empty_subject, &context()),
        digest(&absent_body, &context())
    );

    let chat = Message::chat(None).with_body(Lang::new(), "one".to_owned());
    assert_ne!(digest(&chat, &context()), base_digest);

    let mut thread = base.clone();
    thread.thread = Some(Thread {
        id: "thread".to_owned(),
        parent: None,
    });
    let thread_digest = digest(&thread, &context());
    assert_ne!(thread_digest, base_digest);
    thread.thread = Some(Thread {
        id: "thread".to_owned(),
        parent: Some("parent".to_owned()),
    });
    assert_ne!(digest(&thread, &context()), thread_digest);

    let mut replied = base.clone();
    replied.payloads.push(reply("message-1", None));
    let reply_digest = digest(&replied, &context());
    assert_ne!(reply_digest, base_digest);
    replied.payloads = vec![reply("message-1", Some("alice@example/resource"))];
    assert_ne!(digest(&replied, &context()), reply_digest);
}

#[test]
fn target_and_stanza_language_are_sensitive() {
    let message = body_message("body");
    let absent = digest(&message, &context());
    let bare_context = DigestContext {
        target: NormalizedTarget::Bare("alice@example".parse().expect("bare JID")),
        ..context()
    };
    let full_context = DigestContext {
        target: NormalizedTarget::Full("alice@example/phone".parse().expect("full JID")),
        ..context()
    };
    assert_ne!(absent, digest(&message, &bare_context));
    assert_ne!(
        digest(&message, &bare_context),
        digest(&message, &full_context)
    );

    let with_lang = DigestContext {
        stanza_lang: Some("nb-NO".to_owned()),
        ..context()
    };
    let other_lang = DigestContext {
        stanza_lang: Some("en".to_owned()),
        ..context()
    };
    assert_ne!(absent, digest(&message, &with_lang));
    assert_ne!(digest(&message, &with_lang), digest(&message, &other_lang));
}

#[test]
fn extension_expanded_names_attributes_and_node_order_are_canonical() {
    let first = Element::builder("x", "urn:example:x")
        .attr(xml_ncname!("z").to_owned(), "last")
        .attr(xml_ncname!("a").to_owned(), "first")
        .append("before")
        .append(payload("child", "urn:example:x", "inside"))
        .append("after")
        .build();
    let second = Element::builder("x", "urn:example:x")
        .attr(xml_ncname!("a").to_owned(), "first")
        .attr(xml_ncname!("z").to_owned(), "last")
        .append("before")
        .append(payload("child", "urn:example:x", "inside"))
        .append("after")
        .build();
    assert_eq!(
        digest(&with_payloads(vec![first]), &context()),
        digest(&with_payloads(vec![second]), &context())
    );

    let reordered = Element::builder("x", "urn:example:x")
        .append(payload("child", "urn:example:x", "inside"))
        .append("before")
        .append("after")
        .build();
    assert_ne!(
        digest(&with_payloads(vec![reordered]), &context()),
        digest(
            &with_payloads(vec![payload("x", "urn:example:x", "beforeafter")]),
            &context()
        )
    );

    let server_body = payload("body", "jabber:server", "body");
    assert_ne!(
        digest(&with_payloads(vec![server_body]), &context()),
        digest(&Message::normal(None), &context())
    );
    assert_ne!(
        digest(
            &with_payloads(vec![payload("other", "urn:xmpp:delay", "x")]),
            &context()
        ),
        digest(&Message::normal(None), &context())
    );
}

#[test]
fn strict_payload_validation_rejects_invalid_shapes() {
    let duplicate_origin = with_payloads(vec![origin("one"), origin("two")]);
    assert_error!(
        DigestInput::from_parsed(&duplicate_origin, &context()),
        DigestInputError::DuplicateOriginId
    );
    let malformed_origin = Element::builder("origin-id", SID)
        .attr(xml_ncname!("id").to_owned(), "one")
        .attr(xml_ncname!("by").to_owned(), "evil")
        .build();
    assert_error!(
        DigestInput::from_parsed(&with_payloads(vec![malformed_origin]), &context()),
        DigestInputError::MalformedOriginId
    );
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![Element::builder("origin-id", SID).build()]),
            &context()
        ),
        DigestInputError::MalformedOriginId
    );
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![Element::builder("origin-id", SID)
                .attr(xml_ncname!("id").to_owned(), "one")
                .append(Element::builder("child", "urn:example:x").build())
                .build()]),
            &context()
        ),
        DigestInputError::MalformedOriginId
    );
    assert!(
        DigestInput::from_parsed(&with_payloads(vec![origin("not-a-uuid")]), &context()).is_ok()
    );

    let forged_context = DigestContext {
        server_authorities: vec!["server.example".parse().expect("bare JID")],
        ..context()
    };
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![stanza_id("id", "server.example")]),
            &forged_context,
        ),
        DigestInputError::ForgedServerStanzaId { .. }
    );

    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![reply("one", None), reply("two", None)]),
            &context()
        ),
        DigestInputError::DuplicateReply
    );
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![Element::builder("reply", REPLY).build()]),
            &context()
        ),
        DigestInputError::ReplyMalformed
    );
    assert_error!(
        DigestInput::from_parsed(&with_payloads(vec![reply("", None)]), &context()),
        DigestInputError::ReplyMalformed
    );
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![Element::builder("reply", REPLY)
                .attr(xml_ncname!("id").to_owned(), "id")
                .attr(xml_ncname!("extra").to_owned(), "x")
                .build()]),
            &context()
        ),
        DigestInputError::ReplyMalformed
    );
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![Element::builder("reply", REPLY)
                .attr(xml_ncname!("id").to_owned(), "id")
                .append("text")
                .build()]),
            &context()
        ),
        DigestInputError::ReplyMalformed
    );
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![reply("id", Some("not a jid"))]),
            &context()
        ),
        DigestInputError::ReplyMalformed
    );

    let mut duplicate_thread = Message::normal(None);
    duplicate_thread.thread = Some(Thread {
        id: "one".to_owned(),
        parent: None,
    });
    duplicate_thread
        .payloads
        .push(payload("thread", CLIENT, "two"));
    assert_error!(
        DigestInput::from_parsed(&duplicate_thread, &context()),
        DigestInputError::DuplicateThread
    );
}

#[test]
fn limits_accept_the_boundary_and_reject_one_over() {
    let accepted = body_message(&"x".repeat(65_536));
    assert!(DigestInput::from_parsed(&accepted, &context()).is_ok());
    let rejected = body_message(&"x".repeat(65_537));
    assert_error!(
        DigestInput::from_parsed(&rejected, &context()),
        DigestInputError::TextLengthExceeded
    );

    let mut accepted_languages = Message::normal(None);
    for index in 0..64 {
        accepted_languages
            .bodies
            .insert(Lang::from(format!("x-{index}")), String::new());
    }
    assert!(DigestInput::from_parsed(&accepted_languages, &context()).is_ok());
    accepted_languages
        .bodies
        .insert(Lang::from("x-over"), String::new());
    assert_error!(
        DigestInput::from_parsed(&accepted_languages, &context()),
        DigestInputError::LangCountExceeded
    );

    let exact_id = with_payloads(vec![origin(&"i".repeat(1_024))]);
    assert!(DigestInput::from_parsed(&exact_id, &context()).is_ok());
    assert_error!(
        DigestInput::from_parsed(&with_payloads(vec![origin(&"i".repeat(1_025))]), &context()),
        DigestInputError::IdLengthExceeded
    );

    let exact_attributes = Element::builder("x", "urn:example:x")
        .attr(xml_ncname!("a00").to_owned(), "x")
        .attr(xml_ncname!("a01").to_owned(), "x")
        .attr(xml_ncname!("a02").to_owned(), "x")
        .attr(xml_ncname!("a03").to_owned(), "x")
        .attr(xml_ncname!("a04").to_owned(), "x")
        .attr(xml_ncname!("a05").to_owned(), "x")
        .attr(xml_ncname!("a06").to_owned(), "x")
        .attr(xml_ncname!("a07").to_owned(), "x")
        .attr(xml_ncname!("a08").to_owned(), "x")
        .attr(xml_ncname!("a09").to_owned(), "x")
        .attr(xml_ncname!("a10").to_owned(), "x")
        .attr(xml_ncname!("a11").to_owned(), "x")
        .attr(xml_ncname!("a12").to_owned(), "x")
        .attr(xml_ncname!("a13").to_owned(), "x")
        .attr(xml_ncname!("a14").to_owned(), "x")
        .attr(xml_ncname!("a15").to_owned(), "x")
        .attr(xml_ncname!("a16").to_owned(), "x")
        .attr(xml_ncname!("a17").to_owned(), "x")
        .attr(xml_ncname!("a18").to_owned(), "x")
        .attr(xml_ncname!("a19").to_owned(), "x")
        .attr(xml_ncname!("a20").to_owned(), "x")
        .attr(xml_ncname!("a21").to_owned(), "x")
        .attr(xml_ncname!("a22").to_owned(), "x")
        .attr(xml_ncname!("a23").to_owned(), "x")
        .attr(xml_ncname!("a24").to_owned(), "x")
        .attr(xml_ncname!("a25").to_owned(), "x")
        .attr(xml_ncname!("a26").to_owned(), "x")
        .attr(xml_ncname!("a27").to_owned(), "x")
        .attr(xml_ncname!("a28").to_owned(), "x")
        .attr(xml_ncname!("a29").to_owned(), "x")
        .attr(xml_ncname!("a30").to_owned(), "x")
        .attr(xml_ncname!("a31").to_owned(), "x")
        .build();
    assert!(
        DigestInput::from_parsed(&with_payloads(vec![exact_attributes.clone()]), &context())
            .is_ok()
    );
    let mut over_attributes = exact_attributes;
    over_attributes.set_attr(
        minidom::rxml::Namespace::NONE,
        xml_ncname!("over").to_owned(),
        "x",
    );
    assert_error!(
        DigestInput::from_parsed(&with_payloads(vec![over_attributes]), &context()),
        DigestInputError::AttrCountExceeded
    );

    let exact_name = "n".repeat(1_024);
    assert!(DigestInput::from_parsed(
        &with_payloads(vec![Element::builder(&exact_name, "urn:example:x").build()]),
        &context(),
    )
    .is_ok());
    assert_error!(
        DigestInput::from_parsed(
            &with_payloads(vec![
                Element::builder("n".repeat(1_025), "urn:example:x").build()
            ]),
            &context(),
        ),
        DigestInputError::NameLengthExceeded
    );

    let mut deep = payload("leaf", "urn:example:x", "x");
    for _ in 0..15 {
        deep = Element::builder("node", "urn:example:x")
            .append(deep)
            .build();
    }
    assert!(DigestInput::from_parsed(&with_payloads(vec![deep.clone()]), &context()).is_ok());
    let too_deep = Element::builder("node", "urn:example:x")
        .append(deep)
        .build();
    assert_error!(
        DigestInput::from_parsed(&with_payloads(vec![too_deep]), &context()),
        DigestInputError::DepthExceeded
    );

    let mut exact_nodes = Element::builder("root", "urn:example:x");
    for _ in 0..1_023 {
        exact_nodes = exact_nodes.append(Element::builder("child", "urn:example:x").build());
    }
    assert!(
        DigestInput::from_parsed(&with_payloads(vec![exact_nodes.build()]), &context()).is_ok()
    );
    let mut over_nodes = Element::builder("root", "urn:example:x");
    for _ in 0..1_024 {
        over_nodes = over_nodes.append(Element::builder("child", "urn:example:x").build());
    }
    assert_error!(
        DigestInput::from_parsed(&with_payloads(vec![over_nodes.build()]), &context()),
        DigestInputError::NodeCountExceeded
    );

    let mut over_preimage = Element::builder("root", "urn:example:x");
    for _ in 0..5 {
        over_preimage =
            over_preimage.append(payload("child", "urn:example:x", &"x".repeat(60_000)));
    }
    assert_error!(
        DigestInput::from_parsed(&with_payloads(vec![over_preimage.build()]), &context()),
        DigestInputError::PreimageTooLarge
    );
}

#[test]
fn jid_normalization_and_unicode_are_not_over_normalized() {
    let message = body_message("café");
    let first = DigestContext {
        target: NormalizedTarget::Bare("alice@EXAMPLE.com".parse().expect("JID")),
        ..context()
    };
    let second = DigestContext {
        target: NormalizedTarget::Bare("alice@example.com".parse().expect("JID")),
        ..context()
    };
    assert_eq!(digest(&message, &first), digest(&message, &second));
    assert_ne!(
        digest(&body_message("café"), &context()),
        digest(&body_message("café"), &context())
    );
}

#[test]
fn storage_version_is_pinned() {
    assert!(SemanticDigest::from_storage(2, [0; 32]).is_err());
    let digest = SemanticDigest::from_storage(1, [7; 32]).expect("v1 accepted");
    assert_eq!(digest.to_storage().0, DigestVersion::V1.to_storage());
}

fn hand_str(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn hand_base_preimage(
    message_type: u8,
    stanza_lang: Option<&str>,
    target: Option<(u8, &str)>,
    body: Option<&str>,
) -> Vec<u8> {
    let mut bytes = b"waddle:semantic-digest:v1\0".to_vec();
    bytes.push(message_type);
    match stanza_lang {
        Some(value) => {
            bytes.push(1);
            hand_str(&mut bytes, value);
        }
        None => bytes.push(0),
    }
    match target {
        Some((tag, value)) => {
            bytes.push(tag);
            hand_str(&mut bytes, value);
        }
        None => bytes.push(0),
    }
    match body {
        Some(value) => {
            bytes.extend_from_slice(&1u32.to_be_bytes());
            hand_str(&mut bytes, "");
            hand_str(&mut bytes, value);
        }
        None => bytes.extend_from_slice(&0u32.to_be_bytes()),
    }
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.push(0);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes
}

#[test]
fn frozen_v1_golden_vectors_have_independent_hand_preimages() {
    let cases = [
        (
            Message::normal(None),
            context(),
            hand_base_preimage(0, None, None, None),
            "cb38aa4e2ebe08ecc98cfe8c02581e3dbc4bad971e8931257540b47554df07bc",
        ),
        (
            body_message("hello"),
            context(),
            hand_base_preimage(0, None, None, Some("hello")),
            "da8c607585b9ba8ae8a60c922c4039659eb08f80046abeaaafb5a2af55ca2bdd",
        ),
        (
            Message::chat(None).with_body(Lang::new(), "hei".to_owned()),
            DigestContext {
                target: NormalizedTarget::Bare("alice@example".parse().expect("JID")),
                server_authorities: Vec::new(),
                stanza_lang: Some("nb".to_owned()),
            },
            hand_base_preimage(1, Some("nb"), Some((1, "alice@example")), Some("hei")),
            "99cd018687705639f790e337ea2948965b7394c5c30d411969b360c5744e106b",
        ),
    ];
    for (message, context, preimage, expected) in cases {
        let hand = Sha256::digest(preimage);
        assert_eq!(
            hex(&digest(&message, &context)),
            hex(&SemanticDigest::from_storage(1, hand.into()).expect("v1"))
        );
        assert_eq!(hex(&digest(&message, &context)), expected);
    }
}
