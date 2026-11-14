//! The production SpiceDB authorization-ID codec.
//!
//! Every object and subject ID crosses the SpiceDB boundary as `v2_` plus
//! canonical unpadded base64url of its exact UTF-8 bytes. These tests pin the
//! encoding, its injectivity on identifiers that the previous `@`→`|`,
//! `.`→`=` substitution collapsed, fail-closed decoding of unprefixed legacy
//! IDs, the byte limits, and that every request and response conversion of the
//! production backend goes through the codec.
#![cfg(feature = "test-support")]

use std::collections::{HashMap, HashSet};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jid::BareJid;
use kameo::actor::Spawn;
use waddle_server::config::SpiceDbConfig;
use waddle_server::permissions::{
    decode_object, decode_subject, AuthzIdError, AuthzOpaqueId, AuthzUserJid, CheckPermission,
    DeleteTuple, EncodedObjectId, EncodedSubjectId, EnsureSchema, ListRelations, ListSubjects,
    LookupResources, LookupSubjects, Object, ObjectType, Permission, PermissionActor,
    PermissionError, Relation, Subject, SubjectType, Tuple, WriteTuple, AUTHZ_ID_WIRE_PREFIX,
    MAX_AUTHZ_ID_BYTES, MAX_AUTHZ_WIRE_BYTES,
};
use waddle_server::test_support::spicedb_wire;
use waddle_server::ServerConfig;

/// SpiceDB's `ObjectReference.object_id` pattern, without the `*` wildcard.
fn spicedb_object_id_charset(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_AUTHZ_WIRE_BYTES
        && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'|' | b'-' | b'=' | b'+')
        })
}

/// A wire value built without the codec's validation, for decoder tests.
fn raw_wire(value: &str) -> String {
    let mut wire = String::from(AUTHZ_ID_WIRE_PREFIX);
    URL_SAFE_NO_PAD.encode_string(value, &mut wire);
    wire
}

/// The replaced encoding, kept here only to construct colliding inputs.
fn legacy_encoding(id: &str) -> String {
    id.replace('@', "|").replace('.', "=")
}

fn opaque_wire(value: &str) -> String {
    AuthzOpaqueId::new(value)
        .expect("valid opaque id")
        .to_object_wire()
        .as_str()
        .to_string()
}

fn user_wire(value: &str) -> String {
    AuthzUserJid::parse_exact(value)
        .expect("exact bare user JID")
        .to_subject_wire()
        .as_str()
        .to_string()
}

fn codec_error(result: Result<impl std::fmt::Debug, PermissionError>) -> AuthzIdError {
    match result {
        Err(PermissionError::InvalidAuthzId(error)) => error,
        other => panic!("expected a codec rejection, got {other:?}"),
    }
}

#[test]
fn fixed_vectors_pin_the_wire_format() {
    assert_eq!(AUTHZ_ID_WIRE_PREFIX, "v2_");
    assert_eq!(opaque_wire("a"), "v2_YQ");
    assert_eq!(opaque_wire("ab"), "v2_YWI");
    assert_eq!(opaque_wire("abc"), "v2_YWJj");
    assert_eq!(user_wire("alice@example.com"), "v2_YWxpY2VAZXhhbXBsZS5jb20");
    // Identical source bytes encode identically; the SpiceDB type field, not
    // the ID, separates a user subject from an opaque object.
    assert_eq!(
        user_wire("alice@example.com"),
        AuthzOpaqueId::new("alice@example.com")
            .expect("opaque")
            .to_subject_wire()
            .as_str()
    );
}

#[test]
fn identifiers_aliased_by_the_legacy_substitution_stay_distinct() {
    let object_groups: [&[&str]; 3] = [
        &["a@b.c", "a|b=c", "a@b=c", "a|b.c"],
        &[
            "general@conference.example.com",
            "general|conference=example=com",
        ],
        &["v2_YQ", "a"],
    ];
    for group in object_groups {
        let mut seen = HashSet::new();
        for id in group {
            let wire = opaque_wire(id);
            assert!(seen.insert(wire.clone()), "{id:?} collided");
            assert!(spicedb_object_id_charset(&wire));
            let decoded = decode_object(ObjectType::Channel, &wire).expect("round trip");
            assert_eq!(decoded, Object::new(ObjectType::Channel, *id));
        }
    }

    // Both are exact bare JIDs that the legacy mapping sent to one SpiceDB ID.
    let dotted = "a.b@example.org";
    let equals = "a=b@example.org";
    assert_eq!(legacy_encoding(dotted), legacy_encoding(equals));
    assert_ne!(user_wire(dotted), user_wire(equals));
    for jid in [dotted, equals, "a|b@example.org", "x|y=z@example.org"] {
        let decoded =
            decode_subject(SubjectType::User, &user_wire(jid), None).expect("user round trip");
        assert_eq!(decoded, Subject::user(jid));
    }
}

#[test]
fn encoding_is_injective_over_an_exhaustive_adversarial_alphabet() {
    // Every string of length 1..=4 over the delimiters the legacy mapping
    // conflated, base64url's own alphabet, the prefix separator and
    // composed/decomposed Unicode.
    let alphabet = ['a', '@', '.', '|', '=', '_', '-', 'é', 'e', '\u{301}', '🦆'];
    let mut inputs = vec![String::new()];
    let mut all = Vec::new();
    for _ in 0..4 {
        let mut next = Vec::new();
        for prefix in &inputs {
            for character in alphabet {
                let mut value = prefix.clone();
                value.push(character);
                next.push(value);
            }
        }
        all.extend(next.iter().cloned());
        inputs = next;
    }
    assert_eq!(all.len(), 11 + 121 + 1331 + 14641);

    let mut by_wire: HashMap<String, String> = HashMap::new();
    for value in all {
        let wire = opaque_wire(&value);
        assert!(wire.starts_with(AUTHZ_ID_WIRE_PREFIX));
        assert!(spicedb_object_id_charset(&wire), "{wire:?}");
        assert_eq!(
            AuthzOpaqueId::from_wire(&wire)
                .expect("round trip")
                .as_str(),
            value
        );
        if let Some(previous) = by_wire.insert(wire, value.clone()) {
            panic!("{previous:?} and {value:?} share a wire ID");
        }
    }
}

#[test]
fn unicode_bytes_are_preserved_without_normalization() {
    let values = [
        "Room",
        "room",
        "é",
        "e\u{301}",
        "ﬁ",
        "fi",
        "a|b=c@d/e#f:g",
        "\0\n ",
        "🦆",
        "*",
        "space#member",
        "space",
    ];
    let mut wires = HashSet::new();
    for value in values {
        let wire = opaque_wire(value);
        assert!(wires.insert(wire.clone()), "{value:?} collided");
        assert_ne!(wire, "*", "no source ID may become the SpiceDB wildcard");
        assert_eq!(
            AuthzOpaqueId::from_wire(&wire)
                .expect("round trip")
                .as_str(),
            value
        );
    }
}

#[test]
fn user_subjects_must_be_exact_bare_account_jids() {
    for value in [
        "example.org",
        "alice@example.org/phone",
        "@example.org",
        "*",
        "alice@",
    ] {
        assert_eq!(
            AuthzUserJid::parse_exact(value),
            Err(AuthzIdError::InvalidUserJid),
            "{value:?}"
        );
        assert_eq!(
            AuthzUserJid::from_wire(&raw_wire(value)),
            Err(AuthzIdError::InvalidUserJid),
            "{value:?}"
        );
    }
    // Text the JID parser would rewrite is rejected instead of aliased.
    for value in [
        "Alice@EXAMPLE.ORG",
        "e\u{301}@example.org",
        "fußball@example.org",
        "a@faß.example",
    ] {
        assert_eq!(
            AuthzUserJid::parse_exact(value),
            Err(AuthzIdError::UserJidChangedByParser),
            "{value:?}"
        );
        assert_eq!(
            AuthzUserJid::from_wire(&raw_wire(value)),
            Err(AuthzIdError::UserJidChangedByParser),
            "{value:?}"
        );
    }
    let parsed = BareJid::new("Alice@EXAMPLE.ORG").expect("parser output");
    let user = AuthzUserJid::new(parsed).expect("parsed bare JID");
    assert_eq!(user.jid().as_str(), "alice@example.org");
    assert_eq!(
        AuthzUserJid::from_wire(user.to_subject_wire().as_str()).expect("round trip"),
        user
    );
    assert_eq!(
        AuthzUserJid::new(BareJid::new("example.org").expect("domain")),
        Err(AuthzIdError::InvalidUserJid)
    );
}

#[test]
fn accepted_domain_spellings_are_not_canonicalized_by_the_codec() {
    // A limitation, not a policy: principal canonicalization belongs to the
    // identity boundary. The codec keeps parser-accepted spellings apart.
    for (left, right) in [
        ("a@example.org", "a@example.org."),
        ("a@bücher.example", "a@xn--bcher-kva.example"),
    ] {
        assert_ne!(user_wire(left), user_wire(right));
        for value in [left, right] {
            assert_eq!(
                AuthzUserJid::from_wire(&user_wire(value))
                    .expect("round trip")
                    .jid()
                    .as_str(),
                value
            );
        }
    }
}

#[test]
fn identifiers_are_bounded_in_bytes_within_the_spicedb_limit() {
    assert_eq!(MAX_AUTHZ_WIRE_BYTES, 1024);
    for value in [
        "a".repeat(MAX_AUTHZ_ID_BYTES),
        format!("{}a", "é".repeat(382)),
    ] {
        assert_eq!(value.len(), MAX_AUTHZ_ID_BYTES);
        let wire = opaque_wire(&value);
        assert_eq!(wire.len(), 1023);
        assert!(spicedb_object_id_charset(&wire));
        assert_eq!(
            AuthzOpaqueId::from_wire(&wire)
                .expect("round trip")
                .as_str(),
            value
        );
    }
    for value in ["a".repeat(MAX_AUTHZ_ID_BYTES + 1), "é".repeat(383)] {
        assert_eq!(
            AuthzOpaqueId::new(value.as_str()),
            Err(AuthzIdError::TooLong)
        );
        assert_eq!(
            AuthzOpaqueId::from_wire(&raw_wire(&value)),
            Err(AuthzIdError::TooLong)
        );
    }
    assert_eq!(AuthzOpaqueId::new(""), Err(AuthzIdError::Empty));
    assert_eq!(AuthzOpaqueId::from_wire("v2_"), Err(AuthzIdError::Empty));
    assert_eq!(
        AuthzOpaqueId::from_wire(&"a".repeat(MAX_AUTHZ_WIRE_BYTES + 1)),
        Err(AuthzIdError::TooLong)
    );

    // A valid bare JID one byte over the limit is unsupported, not truncated.
    let longest = format!("{}@x", "a".repeat(MAX_AUTHZ_ID_BYTES - 2));
    let too_long = format!("{}@x", "a".repeat(MAX_AUTHZ_ID_BYTES - 1));
    assert_eq!(user_wire(&longest).len(), 1023);
    let parsed = BareJid::new(&too_long).expect("valid bare JID");
    assert_eq!(AuthzUserJid::new(parsed), Err(AuthzIdError::TooLong));
    assert_eq!(
        AuthzUserJid::parse_exact(&too_long),
        Err(AuthzIdError::TooLong)
    );
    assert_eq!(
        codec_error(spicedb_wire::check_subject(&Subject::user(too_long))),
        AuthzIdError::TooLong
    );
}

#[test]
fn legacy_and_noncanonical_ids_fail_closed() {
    // IDs as the replaced encoding wrote them, and other versions.
    for legacy in [
        "alice|example=com",
        "penguin-club",
        "general|conference=example=com",
        "a|b=c",
        "v1_YQ",
        "V2_YQ",
        "v3_YQ",
        "v2YQ",
        "",
    ] {
        assert_eq!(
            AuthzOpaqueId::from_wire(legacy),
            Err(AuthzIdError::UnsupportedVersion),
            "{legacy:?}"
        );
        assert_eq!(
            AuthzUserJid::from_wire(legacy),
            Err(AuthzIdError::UnsupportedVersion),
            "{legacy:?}"
        );
    }
    for malformed in [
        "v2_YQ==", "v2_YQ=", "v2_YR", "v2_YWJ", "v2_A", "v2_YQ\n", "v2_ YQ", "v2_YQ/", "v2_+w",
        "v2_é", "v2_YQ\0",
    ] {
        assert_eq!(
            AuthzOpaqueId::from_wire(malformed),
            Err(AuthzIdError::InvalidEncoding),
            "{malformed:?}"
        );
    }
    assert_eq!(
        AuthzOpaqueId::from_wire("v2__w"),
        Err(AuthzIdError::InvalidUtf8)
    );

    // The backend's read conversions reject legacy IDs rather than decoding
    // or skipping them.
    let legacy_user = legacy_encoding("alice@example.com");
    assert_eq!(
        codec_error(spicedb_wire::read_relationship_subject(
            "user",
            &legacy_user,
            None
        )),
        AuthzIdError::UnsupportedVersion
    );
    assert_eq!(
        codec_error(spicedb_wire::read_relationship_subject(
            "space",
            "penguin-club",
            Some("member")
        )),
        AuthzIdError::UnsupportedVersion
    );
    assert_eq!(
        codec_error(spicedb_wire::read_lookup_subject(
            SubjectType::User,
            &legacy_user
        )),
        AuthzIdError::UnsupportedVersion
    );
    assert_eq!(
        codec_error(spicedb_wire::read_lookup_resource(
            ObjectType::Channel,
            "general|conference=example=com"
        )),
        AuthzIdError::UnsupportedVersion
    );
}

#[test]
fn production_backend_requests_carry_only_encoded_ids() {
    let channel = Object::new(ObjectType::Channel, "general@conference.example.com");
    let user = Subject::user("a.b@example.org");
    let parent = Subject::userset(SubjectType::Space, "penguin.club", "");
    let members = Subject::userset(SubjectType::Space, "penguin.club", "member");
    let channel_wire = EncodedObjectId::encode(&channel).expect("object");
    let user_wire = EncodedSubjectId::encode(&user).expect("user");
    let space_wire = EncodedSubjectId::encode(&parent).expect("space");
    assert_eq!(
        space_wire,
        EncodedSubjectId::encode(&members).expect("userset")
    );

    let resource = spicedb_wire::check_resource(&channel).expect("check resource");
    assert_eq!(resource.object_type, "channel");
    assert_eq!(resource.object_id, channel_wire.as_str());

    let subject = spicedb_wire::check_subject(&user).expect("check subject");
    assert_eq!(subject.object_type, "user");
    assert_eq!(subject.object_id, user_wire.as_str());
    assert_eq!(subject.relation, None);

    let tuple = Tuple::new(channel.clone(), Relation::new("member"), user.clone());
    let relationship = spicedb_wire::relationship(&tuple).expect("relationship");
    assert_eq!(relationship.resource.object_id, channel_wire.as_str());
    assert_eq!(relationship.relation, "member");
    assert_eq!(relationship.subject.object_id, user_wire.as_str());

    let parent_tuple = Tuple::new(channel.clone(), Relation::new("parent"), parent.clone());
    let parent_relationship = spicedb_wire::relationship(&parent_tuple).expect("parent");
    assert_eq!(parent_relationship.subject.object_type, "space");
    assert_eq!(parent_relationship.subject.object_id, space_wire.as_str());
    assert_eq!(parent_relationship.subject.relation, None);

    let userset = spicedb_wire::check_subject(&members).expect("userset subject");
    assert_eq!(userset.object_id, space_wire.as_str());
    assert_eq!(userset.relation.as_deref(), Some("member"));

    for filter in [
        spicedb_wire::exact_filter(&tuple).expect("exact filter"),
        spicedb_wire::object_and_subject_filter(&channel, &user).expect("subject filter"),
    ] {
        assert_eq!(filter.resource_id.as_deref(), Some(channel_wire.as_str()));
        let subject = filter.subject.expect("subject filter");
        assert_eq!(subject.subject_type, "user");
        assert_eq!(subject.subject_id.as_deref(), Some(user_wire.as_str()));
    }
    let relation_filter =
        spicedb_wire::object_and_relation_filter(&channel, "member").expect("relation filter");
    assert_eq!(
        relation_filter.resource_id.as_deref(),
        Some(channel_wire.as_str())
    );
    assert_eq!(relation_filter.subject, None);

    for id in [
        relationship.resource.object_id.as_str(),
        relationship.subject.object_id.as_str(),
        parent_relationship.subject.object_id.as_str(),
    ] {
        assert!(id.starts_with(AUTHZ_ID_WIRE_PREFIX));
        assert!(spicedb_object_id_charset(id));
    }
}

#[test]
fn production_backend_responses_round_trip_through_the_codec() {
    let user = Subject::user("a=b@example.org");
    let wire = spicedb_wire::check_subject(&user).expect("encode");
    assert_eq!(
        spicedb_wire::read_relationship_subject("user", &wire.object_id, None).expect("read"),
        user
    );
    assert_eq!(
        spicedb_wire::read_lookup_subject(SubjectType::User, &wire.object_id).expect("lookup"),
        user
    );

    let members = Subject::userset(SubjectType::Space, "a|b=c", "member");
    let wire = spicedb_wire::check_subject(&members).expect("encode userset");
    assert_eq!(
        spicedb_wire::read_relationship_subject("space", &wire.object_id, Some("member"))
            .expect("read userset"),
        members
    );

    for id in ["a@b.c", "a|b=c", "🦆.room", "v2_YQ"] {
        let object = Object::new(ObjectType::Channel, id);
        let wire = spicedb_wire::check_resource(&object).expect("encode object");
        assert_eq!(
            spicedb_wire::read_lookup_resource(ObjectType::Channel, &wire.object_id)
                .expect("lookup resource"),
            object
        );
    }
}

#[test]
fn production_backend_rejects_unencodable_identifiers_before_any_request() {
    let channel = Object::new(ObjectType::Channel, "general");
    for (user, expected) in [
        ("alice@example.org/phone", AuthzIdError::InvalidUserJid),
        ("example.org", AuthzIdError::InvalidUserJid),
        ("*", AuthzIdError::InvalidUserJid),
        ("Alice@example.org", AuthzIdError::UserJidChangedByParser),
        ("", AuthzIdError::Empty),
    ] {
        let subject = Subject::user(user);
        assert_eq!(
            codec_error(spicedb_wire::check_subject(&subject)),
            expected,
            "{user:?}"
        );
        let tuple = Tuple::new(channel.clone(), Relation::new("member"), subject.clone());
        assert_eq!(codec_error(spicedb_wire::relationship(&tuple)), expected);
        assert_eq!(codec_error(spicedb_wire::exact_filter(&tuple)), expected);
        assert_eq!(
            codec_error(spicedb_wire::object_and_subject_filter(&channel, &subject)),
            expected
        );
    }
    let empty = Object::new(ObjectType::Space, "");
    assert_eq!(
        codec_error(spicedb_wire::check_resource(&empty)),
        AuthzIdError::Empty
    );
    assert_eq!(
        codec_error(spicedb_wire::object_and_relation_filter(&empty, "member")),
        AuthzIdError::Empty
    );
}

/// Exercises the codec against a real SpiceDB through the production actor.
/// Point it at a disposable instance only: it rewrites the schema.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT and WADDLE_TEST_SPICEDB_PRESHARED_KEY"]
async fn live_spicedb_keeps_legacy_colliding_identities_distinct() {
    let endpoint =
        std::env::var("WADDLE_TEST_SPICEDB_ENDPOINT").expect("WADDLE_TEST_SPICEDB_ENDPOINT");
    let preshared_key = std::env::var("WADDLE_TEST_SPICEDB_PRESHARED_KEY")
        .expect("WADDLE_TEST_SPICEDB_PRESHARED_KEY");
    let mut config = ServerConfig::test_homeserver();
    config.spicedb = Some(SpiceDbConfig {
        endpoint,
        preshared_key,
        insecure: true,
    });
    let backend = PermissionActor::from_server_config(&config)
        .await
        .expect("production SpiceDB backend");
    assert_eq!(backend.backend_name(), "spicedb");
    let actor = PermissionActor::spawn(backend);
    actor.ask(EnsureSchema).await.expect("schema");

    let tag = uuid::Uuid::new_v4().simple().to_string();
    let channel = Object::new(ObjectType::Channel, format!("room.{tag}"));
    let aliased_channel = Object::new(ObjectType::Channel, format!("room={tag}"));
    let granted = Subject::user(format!("a.b@{tag}.example"));
    let aliased_user = Subject::user(format!("a=b@{tag}.example"));
    assert_eq!(
        legacy_encoding(&granted.id),
        legacy_encoding(&aliased_user.id)
    );
    assert_eq!(
        legacy_encoding(&channel.id),
        legacy_encoding(&aliased_channel.id)
    );

    let grant = Tuple::new(channel.clone(), Relation::new("member"), granted.clone());
    actor
        .ask(WriteTuple {
            tuple: grant.clone(),
        })
        .await
        .expect("write grant");

    let check = |subject: &Subject, object: &Object| CheckPermission {
        subject: subject.clone(),
        permission: Permission::Read,
        object: object.clone(),
    };
    assert!(
        actor
            .ask(check(&granted, &channel))
            .await
            .expect("check")
            .allowed
    );
    assert!(
        !actor
            .ask(check(&aliased_user, &channel))
            .await
            .expect("check")
            .allowed
    );
    assert!(
        !actor
            .ask(check(&granted, &aliased_channel))
            .await
            .expect("check")
            .allowed
    );

    let members = actor
        .ask(ListSubjects {
            object: channel.clone(),
            relation: Relation::new("member"),
        })
        .await
        .expect("list subjects");
    assert_eq!(members, vec![granted.clone()]);
    let readers = actor
        .ask(LookupSubjects {
            object: channel.clone(),
            permission: Permission::Read,
            subject_type: SubjectType::User,
        })
        .await
        .expect("lookup subjects");
    assert_eq!(readers, vec![granted.clone()]);
    let relations = actor
        .ask(ListRelations {
            subject: granted.clone(),
            object: channel.clone(),
        })
        .await
        .expect("list relations");
    assert_eq!(relations, vec![Relation::new("member")]);
    let resources = actor
        .ask(LookupResources {
            subject: granted.clone(),
            permission: Permission::Read,
            object_type: ObjectType::Channel,
        })
        .await
        .expect("lookup resources");
    assert!(resources.contains(&channel));
    assert!(!resources.contains(&aliased_channel));
    let aliased_resources = actor
        .ask(LookupResources {
            subject: aliased_user.clone(),
            permission: Permission::Read,
            object_type: ObjectType::Channel,
        })
        .await
        .expect("lookup aliased resources");
    assert!(!aliased_resources.contains(&channel));

    actor
        .ask(DeleteTuple { tuple: grant })
        .await
        .expect("delete grant");
    assert!(
        !actor
            .ask(check(&granted, &channel))
            .await
            .expect("check after delete")
            .allowed
    );
}
