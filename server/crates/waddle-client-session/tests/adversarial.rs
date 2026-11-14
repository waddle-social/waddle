//! Adversarial cases for the timeline: what another occupant or a DM peer
//! can make the store do by choosing ids, reusing a nick, or racing a
//! fastening ahead of its target. Each case reproduces a defect found in
//! review of the first version of this store.

use waddle_client_session::{
    AuthorIdentityPolicy, Delivery, HistoryPage, IncomingMessage, LocalEcho, ReactionUpdate,
    Timeline, TimelineDiff,
};

/// A DM archive row as session.rs::archived_from builds it.
fn dm(mam: &str, origin: &str, own: bool, body: &str, at: i64) -> IncomingMessage {
    IncomingMessage {
        stanza_id: Some(mam.into()),
        origin_id: Some(origin.into()),
        message_id: Some(origin.into()),
        reference_id: Some(origin.into()),
        sender: if own { "david".into() } else { "ann".into() },
        author_id: Some(if own {
            "david@w.s".into()
        } else {
            "ann@w.s".into()
        }),
        own,
        body: Some(body.into()),
        timestamp_ms: at,
        ..IncomingMessage::default()
    }
}

/// A room archive row: reference = mam id, author_id = real JID.
fn room(mam: &str, origin: &str, nick: &str, real: &str, body: &str, at: i64) -> IncomingMessage {
    IncomingMessage {
        stanza_id: Some(mam.into()),
        origin_id: Some(origin.into()),
        message_id: Some(origin.into()),
        reference_id: Some(mam.into()),
        sender: nick.into(),
        author_id: Some(real.into()),
        body: Some(body.into()),
        timestamp_ms: at,
        ..IncomingMessage::default()
    }
}

/// DM: the peer's decoy reuses my origin id X. My own retraction of X,
/// held because my message is on an older page, is dropped: by_sender
/// misses by_chosen, falls back to referenced(X) = the decoy, the author
/// check fails, and nothing is held.
#[test]
fn dm_decoy_swallows_my_held_retraction() {
    let mut t = Timeline::new();
    t.apply_history_page(
        vec![
            dm("m5", "X", false, "decoy", 5),
            IncomingMessage {
                retracts_id: Some("X".into()),
                body: None,
                ..dm("m6", "r6", true, "", 6)
            },
        ],
        Some("m5".into()),
        HistoryPage::Partial,
    );
    t.apply_history_page(
        vec![dm("m1", "X", true, "my secret", 1)],
        Some("m1".into()),
        HistoryPage::Complete,
    );
    let mine = t.items().iter().find(|i| i.own).expect("my row");
    assert!(mine.retracted, "my retraction was lost: {mine:?}");
}

#[test]
fn dm_decoy_swallows_my_held_correction() {
    let mut t = Timeline::new();
    t.apply_history_page(
        vec![
            dm("m5", "X", false, "decoy", 5),
            IncomingMessage {
                replaces_id: Some("X".into()),
                ..dm("m6", "r6", true, "fixed", 6)
            },
        ],
        Some("m5".into()),
        HistoryPage::Partial,
    );
    t.apply_history_page(
        vec![dm("m1", "X", true, "fxied", 1)],
        Some("m1".into()),
        HistoryPage::Complete,
    );
    let mine = t.items().iter().find(|i| i.own).expect("my row");
    assert_eq!(mine.body, "fixed");
}

/// DM: reactions to my message X are also tallied on the peer's decoy,
/// because tally() reads reactions by the row's own reference, not by
/// which row by_reference resolves X to.
#[test]
fn dm_decoy_also_shows_reactions_to_my_message() {
    let mut t = Timeline::new();
    t.apply_history_page(
        vec![
            dm("m1", "X", true, "mine", 1),
            dm("m5", "X", false, "decoy", 5),
            IncomingMessage {
                sender: "ann".into(),
                timestamp_ms: 6,
                reaction: Some(ReactionUpdate {
                    target_id: "X".into(),
                    emojis: vec!["❤️".into()],
                }),
                ..IncomingMessage::default()
            },
        ],
        Some("m1".into()),
        HistoryPage::Complete,
    );
    let decoy = t.items().iter().find(|i| !i.own).expect("decoy");
    assert!(
        decoy.reactions.is_empty(),
        "decoy shows my message's reactions: {:?}",
        decoy.reactions
    );
}

/// Room, history: Mallory later took Bob's freed nick. Her retraction of
/// Bob's message (sender "bob", author_id mallory) overwrites Bob's own
/// held retraction under the same (nick, target) key, and is then rejected
/// by author_id, so Bob's deleted message shows again.
#[test]
fn nick_retaker_overwrites_held_retraction() {
    let mut t = Timeline::new();
    t.apply_history_page(
        vec![
            IncomingMessage {
                retracts_id: Some("S1".into()),
                body: None,
                ..room("S2", "o2", "bob", "bob@w.s", "", 2)
            },
            IncomingMessage {
                retracts_id: Some("S1".into()),
                body: None,
                ..room("S3", "o3", "bob", "mallory@w.s", "", 3)
            },
        ],
        Some("S2".into()),
        HistoryPage::Partial,
    );
    t.apply_history_page(
        vec![room(
            "S1",
            "ob1",
            "bob",
            "bob@w.s",
            "my password is hunter2",
            1,
        )],
        Some("S1".into()),
        HistoryPage::Complete,
    );
    assert!(
        t.items()[0].retracted,
        "bob's retraction lost: {:?}",
        t.items()[0]
    );
}

#[test]
fn nick_retaker_overwrites_held_correction() {
    let mut t = Timeline::new();
    t.apply_history_page(
        vec![
            IncomingMessage {
                replaces_id: Some("ob1".into()),
                ..room("S2", "o2", "bob", "bob@w.s", "meet at 3pm", 2)
            },
            IncomingMessage {
                replaces_id: Some("ob1".into()),
                ..room("S3", "o3", "bob", "mallory@w.s", "x", 3)
            },
        ],
        Some("S2".into()),
        HistoryPage::Partial,
    );
    t.apply_history_page(
        vec![room("S1", "ob1", "bob", "bob@w.s", "meet at 1pm", 1)],
        Some("S1".into()),
        HistoryPage::Complete,
    );
    assert_eq!(t.items()[0].body, "meet at 3pm");
}

/// Same sender, same @id (no origin-id), two different verified stanza
/// ids: the second message is dropped even though the room says they
/// are different messages.
#[test]
fn same_sender_reused_message_id_with_distinct_stanza_ids_drops_one() {
    let mut t = Timeline::new();
    let a = IncomingMessage {
        origin_id: None,
        message_id: Some("1".into()),
        ..room("S1", "x", "bot", "bot@w.s", "one", 1)
    };
    let b = IncomingMessage {
        origin_id: None,
        message_id: Some("1".into()),
        ..room("S2", "x", "bot", "bot@w.s", "two", 2)
    };
    t.apply_history_page(vec![a], Some("S1".into()), HistoryPage::Partial);
    t.apply_live(b);
    assert_eq!(t.items().len(), 2);
}

/// Sanity: echo, reflection, then an older page reindexes; restore,
/// delivery and dedupe still find the row.
#[test]
fn echo_reflection_survives_reindex() {
    let mut t = Timeline::new();
    t.add_local_echo(LocalEcho {
        origin_id: "o1".into(),
        sender: "david".into(),
        body: "hi".into(),
        timestamp_ms: 10,
        reference_id: None,
        reply_to: None,
        attachments: Vec::new(),
    });
    let d = t.apply_live(IncomingMessage {
        own: true,
        author_id: None,
        ..room("S9", "o1", "david", "", "hi", 11)
    });
    assert!(
        matches!(d.as_slice(), [TimelineDiff::Set { index: 0, .. }]),
        "{d:?}"
    );
    t.apply_history_page(
        vec![room("S0", "o0", "ann", "ann@w.s", "earlier", 1)],
        Some("S0".into()),
        HistoryPage::Partial,
    );
    let d = t.apply_history_page(
        vec![IncomingMessage {
            own: true,
            ..room("S9", "o1", "david", "david@w.s", "hi", 11)
        }],
        None,
        HistoryPage::Complete,
    );
    assert!(
        matches!(d.as_slice(), [TimelineDiff::Set { index: 1, item }]
        if item.author_id.as_deref() == Some("david@w.s")),
        "{d:?}"
    );
    assert_eq!(t.items().len(), 2);
    assert!(t.set_delivery("o1", Delivery::Failed).is_empty());
    assert_eq!(t.get("S9").map(|i| i.body.as_str()), Some("hi"));
}

/// Pending moderation keyed by stanza id applies to an own echo once the
/// reflection supplies the stanza id.
#[test]
fn pending_moderation_applies_after_reflection() {
    let mut t = Timeline::new();
    t.add_local_echo(LocalEcho {
        origin_id: "o1".into(),
        sender: "david".into(),
        body: "hi".into(),
        timestamp_ms: 10,
        reference_id: None,
        reply_to: None,
        attachments: Vec::new(),
    });
    t.apply_live(IncomingMessage {
        moderates_id: Some("S9".into()),
        ..IncomingMessage::default()
    });
    t.apply_live(IncomingMessage {
        own: true,
        author_id: None,
        ..room("S9", "o1", "david", "", "hi", 11)
    });
    assert!(t.items()[0].retracted);
}

#[test]
fn dm_decoy_keeps_reaction_after_real_target_pages_in() {
    let mut t = Timeline::new();
    t.apply_history_page(
        vec![
            dm("m5", "X", false, "decoy", 5),
            IncomingMessage {
                sender: "ann".into(),
                timestamp_ms: 6,
                reaction: Some(ReactionUpdate {
                    target_id: "X".into(),
                    emojis: vec!["❤️".into()],
                }),
                ..IncomingMessage::default()
            },
        ],
        Some("m5".into()),
        HistoryPage::Partial,
    );
    t.apply_history_page(
        vec![dm("m1", "X", true, "mine", 1)],
        Some("m1".into()),
        HistoryPage::Complete,
    );
    let counts: Vec<(bool, usize)> = t
        .items()
        .iter()
        .map(|i| (i.own, i.reactions.len()))
        .collect();
    assert_eq!(
        counts,
        [(true, 1), (false, 0)],
        "reaction shown on both rows"
    );
}

#[test]
fn unstamped_nickname_reuser_cannot_mutate_a_known_author_immediately_or_before_history() {
    for buffered in [false, true] {
        for retract in [false, true] {
            let mut timeline = Timeline::with_author_identity_policy(
                AuthorIdentityPolicy::KnownTargetRequiresStableIdentity,
            );
            let original = room(
                "archive-original",
                "origin-original",
                "alex",
                "alex@example.org",
                "Original text",
                10,
            );
            let mutation = IncomingMessage {
                sender: "alex".into(),
                author_id: None,
                body: Some("Forged edit".into()),
                timestamp_ms: 20,
                replaces_id: (!retract).then(|| "origin-original".into()),
                retracts_id: retract.then(|| "origin-original".into()),
                ..IncomingMessage::default()
            };
            if buffered {
                timeline.apply_live(mutation);
                timeline.apply_history_page(
                    vec![original],
                    Some("archive-original".into()),
                    HistoryPage::Complete,
                );
            } else {
                timeline.apply_history_page(
                    vec![original],
                    Some("archive-original".into()),
                    HistoryPage::Complete,
                );
                timeline.apply_live(mutation);
            }
            assert_eq!(
                timeline.items()[0].body,
                "Original text",
                "buffered={buffered}, retract={retract}"
            );
            assert!(!timeline.items()[0].edited);
            assert!(!timeline.items()[0].retracted);
        }
    }
}

#[test]
fn genuinely_anonymous_nickname_bound_corrections_and_retractions_remain_supported() {
    for policy in [
        AuthorIdentityPolicy::NicknameFallback,
        AuthorIdentityPolicy::KnownTargetRequiresStableIdentity,
    ] {
        let mut timeline = Timeline::with_author_identity_policy(policy);
        timeline.apply_live(IncomingMessage {
            author_id: None,
            ..room(
                "archive-original",
                "origin-original",
                "alex",
                "",
                "Original text",
                10,
            )
        });
        timeline.apply_live(IncomingMessage {
            sender: "alex".into(),
            replaces_id: Some("origin-original".into()),
            body: Some("Anonymous correction".into()),
            timestamp_ms: 20,
            ..IncomingMessage::default()
        });
        assert_eq!(timeline.items()[0].body, "Anonymous correction");
        timeline.apply_live(IncomingMessage {
            sender: "alex".into(),
            retracts_id: Some("origin-original".into()),
            timestamp_ms: 30,
            ..IncomingMessage::default()
        });
        assert!(timeline.items()[0].retracted);
    }
}

#[test]
fn legacy_adapters_keep_their_author_fallback_until_they_opt_in() {
    let mut timeline = Timeline::new();
    timeline.apply_live(room(
        "archive-original",
        "original",
        "alex",
        "alex@example.org",
        "Old adapter",
        10,
    ));
    timeline.apply_live(IncomingMessage {
        sender: "alex".into(),
        replaces_id: Some("original".into()),
        body: Some("Legacy correction".into()),
        timestamp_ms: 20,
        ..IncomingMessage::default()
    });
    assert_eq!(timeline.items()[0].body, "Legacy correction");
}

#[test]
fn exact_archive_identity_enriches_unknown_author_without_overwrite_or_downgrade() {
    let mut timeline = Timeline::with_author_identity_policy(
        AuthorIdentityPolicy::KnownTargetRequiresStableIdentity,
    );
    let original = room(
        "archive-original",
        "original",
        "alex",
        "alex@example.org",
        "Original",
        10,
    );
    timeline.apply_live(IncomingMessage {
        author_id: None,
        ..original.clone()
    });
    let diffs = timeline.apply_history_page(
        vec![original.clone()],
        Some("archive-original".into()),
        HistoryPage::Complete,
    );
    assert_eq!(
        timeline.items()[0].author_id.as_deref(),
        Some("alex@example.org")
    );
    assert!(diffs
        .iter()
        .any(|diff| matches!(diff, TimelineDiff::Set { .. })));
    timeline.apply_live(IncomingMessage {
        sender: "alex".into(),
        replaces_id: Some("original".into()),
        body: Some("Unstamped edit".into()),
        timestamp_ms: 20,
        ..IncomingMessage::default()
    });
    assert_eq!(timeline.items()[0].body, "Original");
    timeline.apply_history_page(
        vec![IncomingMessage {
            author_id: Some("mallory@example.org".into()),
            ..original.clone()
        }],
        None,
        HistoryPage::Complete,
    );
    timeline.apply_history_page(
        vec![IncomingMessage {
            author_id: None,
            ..original
        }],
        None,
        HistoryPage::Complete,
    );
    assert_eq!(
        timeline.items()[0].author_id.as_deref(),
        Some("alex@example.org")
    );
}

#[test]
fn sender_chosen_id_alone_cannot_enrich_an_unknown_author() {
    let mut timeline = Timeline::with_author_identity_policy(
        AuthorIdentityPolicy::KnownTargetRequiresStableIdentity,
    );
    let original = room(
        "archive-original",
        "original",
        "alex",
        "alex@example.org",
        "Original",
        10,
    );
    timeline.apply_live(IncomingMessage {
        stanza_id: None,
        author_id: None,
        ..original.clone()
    });
    timeline.apply_history_page(vec![original], None, HistoryPage::Complete);
    assert_eq!(timeline.items().len(), 1);
    assert!(timeline.items()[0].author_id.is_none());
}
