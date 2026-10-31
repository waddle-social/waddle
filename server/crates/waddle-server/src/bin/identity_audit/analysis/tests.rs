use super::{accounts, frames, CollisionBasis};
use crate::model::{AccountRow, AccountSource, Finding, OriginalValue, RecordKey};
use crate::profile::candidate_findings;

fn row(source: AccountSource, node: &str, domain: &str, key: Option<&str>) -> AccountRow {
    AccountRow {
        record: match source {
            AccountSource::Oidc => RecordKey::Oidc(OriginalValue::new(format!("{node}@{domain}"))),
            AccountSource::Native => RecordKey::Native(7),
        },
        source,
        localpart: OriginalValue::new(node.to_owned()),
        domain: OriginalValue::new(domain.to_owned()),
        stored_key: key.map(|key| OriginalValue::new(key.to_owned())),
    }
}

#[test]
fn cross_store_twins_are_findings_not_a_single_account() {
    let (observations, collisions) = accounts(vec![
        row(
            AccountSource::Oidc,
            "alice",
            "home.example.test",
            Some("alice"),
        ),
        row(
            AccountSource::Native,
            "alice",
            "home.example.test",
            Some("alice@home.example.test"),
        ),
    ]);
    assert_eq!(observations.len(), 2);
    assert_eq!(collisions.len(), 3);
    assert!(collisions
        .iter()
        .all(|group| group.account_indexes == [0, 1]));
}

#[test]
fn sharp_s_parser_collision_is_not_hidden_by_an_ascii_prepared_key() {
    let (observations, collisions) = accounts(vec![
        row(
            AccountSource::Oidc,
            "fußball",
            "home.example.test",
            Some("fussball"),
        ),
        row(
            AccountSource::Native,
            "fussball",
            "home.example.test",
            Some("fussball@home.example.test"),
        ),
    ]);
    assert_eq!(collisions.len(), 3);
    assert!(observations[0].findings.contains(&Finding::UnicodeInput));
    assert!(observations[0]
        .findings
        .contains(&Finding::ChangedByCurrentParser));
    assert!(observations[0]
        .findings
        .contains(&Finding::SourceJidDisagrees));
}

#[test]
fn trailing_dot_and_domain_labels_stay_unresolved_instead_of_silently_merged() {
    let (observations, collisions) = accounts(vec![
        row(AccountSource::Native, "a", "home.example.test", None),
        row(AccountSource::Native, "a", "home.example.test.", None),
        row(AccountSource::Native, "a", "bücher.example.test", None),
        row(
            AccountSource::Native,
            "a",
            "xn--bcher-kva.example.test",
            None,
        ),
    ]);
    assert_eq!(collisions.len(), 1);
    assert!(matches!(
        collisions[0].basis,
        CollisionBasis::AccountPreparation
    ));
    assert_eq!(collisions[0].account_indexes, [0, 1]);
    assert!(observations[1]
        .findings
        .contains(&Finding::PreparationPathsDisagree));
    assert_eq!(
        observations[1]
            .current_account_prepared_jid
            .as_ref()
            .expect("account preparation")
            .as_str(),
        "a@home.example.test"
    );
    assert_eq!(
        observations[1]
            .current_stanza_parsed_jid
            .as_ref()
            .expect("stanza parsing")
            .as_str(),
        "a@home.example.test."
    );
    assert!(observations[1]
        .findings
        .contains(&Finding::CandidateTrailingDot));
    assert!(observations[2].findings.contains(&Finding::UnicodeInput));
    assert!(observations[3].findings.contains(&Finding::CandidateALabel));
    assert!(observations
        .iter()
        .all(|row| row.findings.contains(&Finding::MissingPreparedKey)));
}

#[test]
fn recorded_binding_collision_and_incompatible_key_are_both_visible() {
    let (observations, collisions) = accounts(vec![
        row(
            AccountSource::Oidc,
            "alice",
            "home.example.test",
            Some("bob"),
        ),
        row(
            AccountSource::Native,
            "bob",
            "home.example.test",
            Some("bob@home.example.test"),
        ),
    ]);
    assert!(observations[0]
        .findings
        .contains(&Finding::IncompatiblePreparedKey));
    assert_eq!(collisions.len(), 1);
}

#[test]
fn candidate_has_explicit_limits_and_never_normalizes_input() {
    assert!(candidate_findings("alice_1", "home.example.test").is_empty());
    for (node, domain) in [
        ("Alice", "home.example.test"),
        ("a", "Home.example.test"),
        ("a", "-home.example.test"),
        ("a", "home..test"),
        ("a|b", "home.example.test"),
        ("a", "xn--bcher-kva.example.test"),
    ] {
        assert!(!candidate_findings(node, domain).is_empty());
    }
    assert!(!candidate_findings(&"a".repeat(65), "home.example.test").is_empty());
}

#[test]
fn frame_corpus_keeps_decoded_and_actual_prepared_values_separate() {
    let audit =
        frames(&["<message xmlns='jabber:client' to='a@fa&#223;.example.test'/>".to_owned()])
            .expect("frame");
    assert_eq!(
        audit[0]
            .to
            .original_xml_decoded
            .as_ref()
            .expect("original")
            .as_str(),
        "a@faß.example.test"
    );
    assert_eq!(
        audit[0]
            .to
            .current_parser_jid
            .as_ref()
            .expect("prepared")
            .as_str(),
        "a@fass.example.test"
    );
    assert!(audit[0].to.findings.contains(&Finding::UnicodeInput));
    assert!(audit[0].from.original_xml_decoded.is_none());
    assert!(frames(&["<message xmlns='jabber:client'/>", "<bad/>"].map(str::to_owned)).is_err());
}
