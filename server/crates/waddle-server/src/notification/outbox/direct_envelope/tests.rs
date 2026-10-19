use super::*;

fn bare(value: &str) -> BareJid {
    value.parse().expect("valid bare JID")
}

fn message_with_mention(mention: waddle_xmpp::xep::ExplicitMention) -> Message {
    let mut message = Message::new(None::<Jid>);
    message
        .payloads
        .push(waddle_xmpp::xep::build_mention_element(&mention));
    message
}

/// Dedup regression: `mention_bits_for_recipient` MUST derive
/// both `is_mention` and `noping` from a single
/// `extract_explicit_mentions` parse, and behavior MUST match the
/// pre-dedup two-helper shape (mention bit set whenever a JID
/// matches, noping bit set independently when `<noping/>` is
/// present on the matching mention).
#[test]
fn dm_mention_bits_single_parse_matches_pre_dedup_behavior() {
    let recipient = bare("alice@example.com");

    // No mentions → both bits unset.
    let no_mention = Message::new(None::<Jid>);
    assert_eq!(
        mention_bits_for_recipient(&no_mention, &recipient),
        RecipientMentionBits::default(),
    );

    // Plain mention naming the recipient → mention set, noping unset.
    let plain = message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(recipient.clone()));
    assert_eq!(
        mention_bits_for_recipient(&plain, &recipient),
        RecipientMentionBits {
            is_mention: true,
            noping: false,
        },
    );

    // `<noping/>` mention naming the recipient → BOTH bits set;
    // matches the original `ExplicitMentions::mentions_jid`
    // semantics (which ignored `<noping/>` when deriving `is_mention`).
    let mut noping = waddle_xmpp::xep::ExplicitMention::jid(recipient.clone());
    noping.noping = true;
    let msg_noping = message_with_mention(noping);
    assert_eq!(
        mention_bits_for_recipient(&msg_noping, &recipient),
        RecipientMentionBits {
            is_mention: true,
            noping: true,
        },
    );

    // Mention naming someone else → both bits unset.
    let other = bare("bob@example.com");
    let foreign = message_with_mention(waddle_xmpp::xep::ExplicitMention::jid(other));
    assert_eq!(
        mention_bits_for_recipient(&foreign, &recipient),
        RecipientMentionBits::default(),
    );
}

/// XEP-0513 §304 SHOULD: when the per-message count exceeds the
/// threshold, the recipient's mention class is downgraded
/// (is_mention → false, class drops from `dm_mention` to plain
/// `dm`). The `<noping/>` suppression is NOT cleared — XEP-0513
/// §"No Ping" is a separate SHOULD that operates independently
/// of the count cap. Compliance review on PR #741 mandated this
/// composition.
#[test]
fn xep0513_dm_mention_count_exceeded_downgrades_is_mention_but_preserves_noping() {
    let recipient = bare("alice@example.com");
    let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

    // Case A: count exceeded, NO `<noping/>` for recipient → both
    // bits collapse (is_mention forced false; no noping to set).
    let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    msg.payloads.push(waddle_xmpp::xep::build_mention_element(
        &waddle_xmpp::xep::ExplicitMention::jid(recipient.clone()),
    ));
    for i in 0..threshold {
        msg.payloads.push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::jid(
                format!("user{i}@example.com").parse().expect("user bare"),
            ),
        ));
    }
    let bits = mention_bits_for_recipient(&msg, &recipient);
    assert!(
        !bits.is_mention,
        "count exceeded MUST downgrade is_mention to false (§304 \
         ignore-all-mentions for the class decision)"
    );
    assert!(
        !bits.noping,
        "no `<noping/>` for the recipient → noping bit stays false"
    );

    // Case B: count exceeded AND `<noping/>` for recipient → the
    // class downgrades (is_mention=false) BUT the noping bit is
    // preserved so the existing T0/T1 `Xep0513Noping` suppressor
    // still fires for this recipient.
    let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    let mut recipient_noping_mention = waddle_xmpp::xep::ExplicitMention::jid(recipient.clone());
    recipient_noping_mention.noping = true;
    msg.payloads.push(waddle_xmpp::xep::build_mention_element(
        &recipient_noping_mention,
    ));
    for i in 0..threshold {
        msg.payloads.push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::jid(
                format!("user{i}@example.com").parse().expect("user bare"),
            ),
        ));
    }
    let bits = mention_bits_for_recipient(&msg, &recipient);
    assert!(
        !bits.is_mention,
        "count exceeded MUST downgrade is_mention to false"
    );
    assert!(
        bits.noping,
        "XEP-0513 §\"No Ping\" SHOULD MUST survive §304 count \
         overflow — `<noping/>` suppresses push candidate creation \
         for this recipient regardless of the count cap"
    );
}

/// At-threshold (exactly `DEFAULT_MENTIONS_COUNT` mentions) MUST
/// preserve the recipient's `<noping/>` suppression. The §304
/// threshold is exclusive ("more than"), so equal-count is still
/// honored.
#[test]
fn xep0513_dm_mention_count_at_threshold_preserves_noping() {
    let recipient = bare("alice@example.com");
    let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;

    let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    let mut recipient_noping = waddle_xmpp::xep::ExplicitMention::jid(recipient.clone());
    recipient_noping.noping = true;
    msg.payloads
        .push(waddle_xmpp::xep::build_mention_element(&recipient_noping));
    // Fill to exactly threshold mentions total.
    for i in 0..(threshold - 1) {
        msg.payloads.push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::jid(
                format!("user{i}@example.com").parse().expect("user bare"),
            ),
        ));
    }
    let bits = mention_bits_for_recipient(&msg, &recipient);
    assert!(
        bits.is_mention,
        "at-threshold count is still honored — is_mention MUST be set"
    );
    assert!(
        bits.noping,
        "at-threshold count is still honored — noping MUST be set"
    );
}

/// Cross-XEP review on PR #741: a DM sender naming the recipient
/// solely via an XEP-0372 `<reference type='mention'/>` (no
/// XEP-0513 `<mention/>`) MUST still set the recipient's
/// `is_mention` bit. Without this the §304 count gate counts
/// XEP-0372 toward the threshold (penalising spam) while the
/// classifier ignores XEP-0372 for promotion (demoting
/// legitimate XEP-0372-only DMs to plain `dm`).
#[test]
fn xep0372_only_dm_mention_promotes_recipient_to_dm_mention() {
    let recipient = bare("alice@example.com");
    let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    waddle_xmpp::xep::add_reference(
        &mut msg,
        &waddle_xmpp::xep::Reference::mention("xmpp:alice@example.com"),
    );
    let bits = mention_bits_for_recipient(&msg, &recipient);
    assert!(
        bits.is_mention,
        "a DM with only an XEP-0372 `<reference type='mention'/>` \
         naming the recipient MUST still set is_mention — the \
         groupchat path already does this via groupchat_mentions_owner"
    );
    assert!(
        !bits.noping,
        "XEP-0372 has no `<noping/>` concept; noping stays false"
    );
}

/// Composition with the §304 count gate: an XEP-0372-only mention
/// still respects the threshold cap. Six references targeting
/// six different users (including the recipient) trip the gate
/// → recipient's `is_mention` is downgraded. XEP-0372 has no
/// `<noping/>` concept so the noping bit is unaffected and the
/// overall outcome matches `RecipientMentionBits::default()`.
#[test]
fn xep0372_dm_mention_count_exceeded_downgrades_is_mention() {
    let recipient = bare("alice@example.com");
    let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    let threshold = waddle_xmpp::xep::DEFAULT_MENTIONS_COUNT;
    // One reference naming the recipient + threshold others = exceed.
    waddle_xmpp::xep::add_reference(
        &mut msg,
        &waddle_xmpp::xep::Reference::mention("xmpp:alice@example.com"),
    );
    for i in 0..threshold {
        waddle_xmpp::xep::add_reference(
            &mut msg,
            &waddle_xmpp::xep::Reference::mention(format!("xmpp:user{i}@example.com")),
        );
    }
    let bits = mention_bits_for_recipient(&msg, &recipient);
    assert!(!bits.is_mention, "is_mention MUST be false on count-exceed");
    assert!(
        !bits.noping,
        "XEP-0372 has no `<noping/>` shape; noping stays false \
         (the §\"No Ping\" preservation rule applies only when the \
         recipient's XEP-0513 mention carries `<noping/>`)"
    );
}
